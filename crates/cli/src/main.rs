//! CLI arguments, command dispatch, source loading, and shutdown handling.
//!
//! `completions` builds shell completions; `replay` plays and exports recordings;
//! `devices` provides audio, MIDI, and gamepad checks and input monitors.
//! `sample_cache` manages cached samples, and `live` runs live and watched playback.
//! `replay_plan` selects recorded saves for playback and export; `style` handles
//! terminal colours.

#[cfg(feature = "device-audio")]
use std::io;
use std::io::Write as _;
use std::io::{IsTerminal as _, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use rustel_fraction::Fraction;
#[cfg(feature = "device-audio")]
use rustel_runtime::CapabilitySafetyFacts;
#[cfg(feature = "osc")]
use rustel_runtime::ScoreOscAccess;
use rustel_runtime::{
    AccelerationPreference, CapabilityReportContext, CapabilityReportV1, RenderFormat,
    RuntimeError, ScoreSampleAccess, Session, SessionConfig, product, sample_cache_dir,
    terminal_text,
};

// The live CPAL callback enters `rustel_audio::tripwire::audio_scope`; using
// the matching allocator lets the device report callback allocations.
// `play_live` verifies the allocator before trusting a zero. Other binaries
// embedding the library choose their own allocator.
#[cfg(feature = "device-audio")]
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

/// The Cargo features this build of `rustel` enables, sorted; `build.rs`
/// reads them from Cargo.
const BUILD_FEATURES: &[&str] = include!(concat!(env!("OUT_DIR"), "/build_features.rs"));

/// Minimal ANSI styling for the human-readable CLI surface.
///
/// Colour turns on only when the target stream is a terminal, `NO_COLOR` is
/// unset or empty, and `TERM` names a terminal that renders escapes, so piped and
/// scripted output stays plain and the `--json` flag always emits raw JSON
/// that styling never touches.
mod style;

mod replay_plan;
use replay_plan::{ReplayOpening, ReplayPlan};

mod completions;
mod devices;
mod live;
mod replay;
mod sample_cache;

use completions::{install_completions, run_completions, shell_from_environment};
#[cfg(all(test, feature = "device-audio"))]
use devices::doctor_device_report;
#[cfg(test)]
use devices::doctor_unavailable_report;
#[cfg(all(test, feature = "midi"))]
use devices::midi_summary_json;
use devices::{run_devices, run_doctor, run_gamepad_monitor, run_midi_list, run_midi_monitor};
#[cfg(feature = "device-audio")]
use live::read_ui_controls;
#[cfg(all(
    test,
    feature = "device-audio",
    any(feature = "midi", feature = "osc", feature = "serial")
))]
use live::remaining_output_delay;
#[cfg(all(test, feature = "device-audio", feature = "midi"))]
use live::sync_live_midi_inputs;
#[cfg(all(
    test,
    feature = "device-audio",
    feature = "midi",
    feature = "osc",
    feature = "serial"
))]
use live::take_tick_intents;
#[cfg(all(test, feature = "device-audio"))]
use live::{
    apply_ui_slider_control, build_live_producer, install_sample_batch, live_engine_pressure_event,
    observe_ui_layout_if_audible, runtime_device_error,
};
use live::{play, play_live, run_musician, watch_file};
#[cfg(feature = "device-audio")]
use replay::announce_active_score;
use replay::{render_active_score, run_replay};
use sample_cache::{confirm_cache_clear, directory_sizes, run_samples_cache, run_samples_clear};

/// The help colours, as cargo uses them: bold green headings, bold cyan
/// literals and cyan placeholders. Clap disables them on a pipe and when
/// `TERM` is `dumb` or absent, as `style` does. `NO_COLOR` disables them
/// everywhere, as for the rest of the output.
fn help_styles() -> clap::builder::Styles {
    use clap::builder::styling::{AnsiColor, Effects, Styles};
    if color_disabled() {
        return Styles::plain();
    }
    Styles::styled()
        .header(AnsiColor::Green.on_default() | Effects::BOLD)
        .usage(AnsiColor::Green.on_default() | Effects::BOLD)
        .literal(AnsiColor::Cyan.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Cyan.on_default())
        .error(AnsiColor::Red.on_default() | Effects::BOLD)
        .valid(AnsiColor::Green.on_default())
        .invalid(AnsiColor::Yellow.on_default())
}

#[derive(Parser, Debug)]
#[command(
    name = product::COMMAND_NAME,
    version = product::VERSION,
    about = "Headless pattern runtime and local file-to-speakers loop",
    long_about = product::CLI_LONG_ABOUT,
    styles = help_styles(),
    after_help = "Example: rustel play song.strudel --watch\n\nDocumentation: https://github.com/tzfm/rustel/blob/main/docs/cli.md\nReport a bug: https://github.com/tzfm/rustel/issues",
    disable_help_flag = true
)]
struct Cli {
    /// Print full help, including examples.
    #[arg(short = 'h', long, global = true, action = clap::ArgAction::HelpLong)]
    help: Option<bool>,
    /// Disable ANSI colour and styling.
    #[arg(long, global = true)]
    no_color: bool,
    /// Never prompt for input; cache deletion requires --force.
    #[arg(long, global = true)]
    no_input: bool,
    /// Disable release checks and update notices for this run.
    #[arg(long, global = true)]
    no_update_check: bool,
    /// Disable ANSI styling and animated progress.
    #[arg(long, global = true)]
    plain: bool,
    /// Suppress notices and progress; keep results and errors.
    #[arg(short = 'q', long, global = true)]
    quiet: bool,
    /// Show progressively more live diagnostics (-v, -vv, -vvv).
    ///
    /// The first two levels stay human-readable. The third emits every live
    /// diagnostic as JSON, including the versioned per-second engine-pressure
    /// report. Score source events remain opt-in with --score-events.
    #[arg(
        short = 'v',
        long,
        action = clap::ArgAction::Count,
        global = true,
        help_heading = "Diagnostics"
    )]
    verbose: u8,
    /// Engine-owned DSP kernels: auto or portable.
    ///
    /// Portable forces the convolution, supersaw and wavetable reference
    /// kernels. It does not disable RustFFT or compiler-generated SIMD.
    #[arg(
        long,
        global = true,
        default_value = "auto",
        value_name = "auto|portable",
        help_heading = "Diagnostics"
    )]
    acceleration: AccelerationPreference,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
#[repr(u8)]
enum LiveDetail {
    Essential = 0,
    Verbose = 1,
    Debug = 2,
}

/// Controls live diagnostics on stderr independently of session recordings.
///
/// `--json` changes the format while verbosity still selects events.
/// The live-UI protocol receives every event, regardless of verbosity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiveOutput {
    verbosity: u8,
    /// The live-UI protocol: every event, filtered by nothing.
    protocol: bool,
    /// Objects rather than sentences.
    json: bool,
}

impl LiveOutput {
    const JSON_VERBOSITY: u8 = 3;

    fn new(verbosity: u8, protocol: bool, json: bool) -> Self {
        Self {
            verbosity,
            protocol,
            json,
        }
    }

    fn event(self, detail: LiveDetail, event: serde_json::Value) {
        if quiet_asked() && !self.protocol {
            return;
        }
        if let Some(line) = self.render(detail, &event) {
            eprintln!("{line}");
        }
    }

    /// Whether what leaves here is machine-readable.
    fn structured(self) -> bool {
        self.protocol || self.json || self.verbosity >= Self::JSON_VERBOSITY
    }

    #[cfg(feature = "device-audio")]
    fn enabled(self, detail: LiveDetail) -> bool {
        self.structured() || self.verbosity >= detail as u8
    }

    fn render(self, detail: LiveDetail, event: &serde_json::Value) -> Option<String> {
        // How much, then in what form. Only the protocol skips the first
        // question: its consumer asked for the stream, not for a reading of
        // it. `-vvv` passes everything here anyway, being past the deepest
        // detail there is, so the shorthand still means what it did.
        if !self.protocol && self.verbosity < detail as u8 {
            return None;
        }
        // Structured JSON is left as is; human lines show control and bidi
        // characters as visible stand-ins.
        if self.structured() {
            return Some(event.to_string());
        }
        human_live_event(event, self.verbosity).map(|line| style::safe_source(&line))
    }
}

fn human_live_event(event: &serde_json::Value, verbosity: u8) -> Option<String> {
    fn string<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
        value.get(key)?.as_str()
    }

    if let Some(live) = event.get("live") {
        return match string(live, "status")? {
            "started" => {
                let path = string(live, "path").unwrap_or("score");
                if verbosity == 0 {
                    Some(format!("Started {path}"))
                } else {
                    let device = string(live, "device").unwrap_or("audio device");
                    let sample_rate = live
                        .get("sample_rate")
                        .and_then(serde_json::Value::as_u64)
                        .map(|rate| format!(" at {rate} Hz"))
                        .unwrap_or_default();
                    Some(format!("Started {path} on {device}{sample_rate}"))
                }
            }
            // A run that ended on its own -- a finished `--duration`, a host
            // calling stop -- says so, because otherwise it just stops
            // printing and leaves the player wondering. An INTERRUPTED one
            // has already said "Playback stopped, letting the tail ring
            // out": a bare "Stopped" under that only repeats the half they
            // had read, so it is left out rather than the line being
            // dropped for everyone.
            "stopped" => (!live
                .get("interrupted")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false))
            .then(|| "Stopped".to_owned()),
            "recycled" => Some(format!(
                "Restarted audio output {}",
                string(live, "device").unwrap_or("device")
            )),
            _ => None,
        };
    }

    if let Some(reload) = event.get("reload") {
        let path = string(reload, "path").unwrap_or("score");
        return match string(reload, "status")? {
            "installed" => Some(format!("Updated {path}")),
            "rejected" => Some(format!(
                "Update failed for {path}: {}",
                string(reload, "message").unwrap_or("the score was rejected")
            )),
            _ => None,
        };
    }

    if let Some(error) = event.get("live_error") {
        let message = string(error, "message").unwrap_or("live playback failed");
        // A `.log()` line is score output, one line per note that sounds.
        // Print the text as the score wrote it. An "Error:" prefix would
        // suggest that the run failed.
        if string(error, "kind") == Some("log") {
            return Some(message.to_owned());
        }
        // A sound the library is still fetching is not a fault of anything:
        // the set is playing and it joins when it lands. This used to print
        // as "Error:" beside the real ones, which on a save that introduces
        // a new sound reads as the save having failed when it took.
        if matches!(
            string(error, "kind"),
            Some(
                rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC
                    | rustel_runtime::SAMPLE_AWAITED_DIAGNOSTIC
            )
        ) {
            return Some(format!("waiting: {message}"));
        }
        // A sound that could not be fetched is skipped and the set plays
        // on: a warning, not an error, so a 404 on one soundfont does not
        // read as the run having died. A score that failed to compile is
        // still an error, recoverable or not: the musician has to act.
        // A serial baud that could not change mid-set is reported once and
        // the writes go out at the open baud: news, not a failure. So are
        // serial writes refused, dropped late or rejected: the set plays on.
        if matches!(
            string(error, "kind"),
            Some("serial-baud" | "serial-trouble")
        ) {
            return Some(format!("warning: {message}"));
        }
        // A serial port still opening is waiting, like a sound still
        // loading: an offline Bluetooth adapter answers in seconds and then
        // works, and its writes wait for it meanwhile.
        if string(error, "kind") == Some("serial-opening") {
            return Some(format!("waiting: {message}"));
        }
        let skipped_sound = string(error, "kind") == Some("sample-failed");
        return Some(if skipped_sound {
            format!("warning: {message} - skipped, the set plays on")
        } else {
            format!("Error: {message}")
        });
    }

    if let Some(clock) = event.get("midi_clock") {
        let port = string(clock, "port").unwrap_or("port");
        return match string(clock, "status")? {
            "out" => Some(format!(
                "MIDI clock out to {port} - the hardware follows rustel"
            )),
            "in" => Some(format!("Following MIDI clock from {port}")),
            "locked" => Some(format!("Locked to the MIDI clock from {port}")),
            "lost" => Some(format!("Lost the MIDI clock from {port}")),
            _ => None,
        };
    }

    // What opened, once: the host, the device and the buffer's real cost.
    // This is essential detail, so the human form prints it at the default
    // verbosity, as the structured form does.
    if let Some(audio) = event.get("audio").and_then(serde_json::Value::as_str) {
        return Some(format!("Audio out {audio}"));
    }

    if let Some(input) = event.get("audio_input") {
        let name = string(input, "device").unwrap_or("input");
        return match string(input, "status")? {
            "open" => Some(
                match input.get("channels").and_then(serde_json::Value::as_u64) {
                    Some(1) => format!("Audio in {name} - 1 channel; s(\"in\") plays it"),
                    Some(channels) => format!(
                        "Audio in {name} - {channels} channels; s(\"in\") plays the first, \
                         in:1 the next"
                    ),
                    None => format!("Audio in {name}"),
                },
            ),
            _ => None,
        };
    }

    #[cfg(feature = "hydra")]
    if let Some(hydra) = event.get("hydra") {
        return Some(format!(
            "Note: {}",
            string(hydra, "message").unwrap_or("this score draws visuals")
        ));
    }

    if let Some(replay) = event.get("replay") {
        let session = string(replay, "session").unwrap_or("session");
        if verbosity == 0 {
            return Some(format!("Replaying {session}"));
        }
        let saves = replay
            .get("saves")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let duration = replay
            .get("duration_secs")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        return Some(format!(
            "Replaying {session} ({saves} saves, {duration:.1}s)"
        ));
    }

    if let Some(save) = event.get("replay_save") {
        let at = save
            .get("t")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        return Some(format!("The save at {at:.3}s was rejected when recorded"));
    }

    if let Some(recording) = event.get("session_recording") {
        if string(recording, "status") == Some("unavailable") {
            return Some(format!(
                "Warning: session recording unavailable: {}",
                string(recording, "message").unwrap_or("unknown error")
            ));
        }
        return Some(format!(
            "Recording session to {}",
            string(recording, "path").unwrap_or("session file")
        ));
    }

    if let Some(samples) = event.get("sample_library") {
        return Some(format!(
            "Warning: sample library unavailable: {}",
            string(samples, "message").unwrap_or("unknown error")
        ));
    }

    if let Some(warmup) = event.get("sample_warmup") {
        return Some(format!(
            "Prepared {} sample files in {} ms",
            warmup
                .get("files")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            warmup
                .get("waited_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        ));
    }

    if let Some(settle) = event.get("device_settle") {
        let waited = settle
            .get("waited_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        return Some(
            if settle
                .get("settled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                format!("Audio device settled in {waited} ms")
            } else {
                format!("Audio device did not settle after {waited} ms")
            },
        );
    }

    if let Some(stopping) = event.get("stopping") {
        return Some(format!(
            "Playback stopped, {}",
            string(stopping, "message").unwrap_or("letting the tail ring out")
        ));
    }

    if let Some(stopped) = event.get("stopped") {
        return Some(format!(
            "Audio tail: {}",
            string(stopped, "message").unwrap_or("finished")
        ));
    }

    if let Some(pressure) = event.get("engine_pressure") {
        let Some(status) = pressure.get("status").filter(|value| !value.is_null()) else {
            return Some("Load: unavailable".into());
        };
        let percent = |section: &str| {
            pressure
                .get(section)
                .and_then(|value| value.get("slow_load_basis_points"))
                .and_then(serde_json::Value::as_u64)
                .map(|basis_points| (basis_points + 50) / 100)
                .unwrap_or(0)
        };
        let voices = pressure.get("voices");
        let active_voices = voices
            .and_then(|value| value.get("active"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let voice_capacity = voices
            .and_then(|value| value.get("semantic_capacity"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let cover_ms = pressure
            .get("scheduler")
            .and_then(|value| value.get("cover_end_nanos"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            / 1_000_000;
        return Some(format!(
            "Load: {} (DSP {}%, scheduler {}%, voices {active_voices}/{voice_capacity}, cover {cover_ms}ms)",
            string(status, "message").unwrap_or("unknown"),
            percent("dsp"),
            percent("scheduler"),
        ));
    }

    if let Some(load) = event.get("live_load") {
        let busy = load
            .get("producer_busy")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0)
            * 100.0;
        return Some(format!(
            "Load: {} ({busy:.1}% producer)",
            string(load, "verdict").unwrap_or("unknown")
        ));
    }

    if let Some(midi) = event.get("midi_published") {
        return Some(format!(
            "Published MIDI port {}",
            string(midi, "port").unwrap_or("unknown")
        ));
    }

    if let Some(capture) = event.get("debug_capture") {
        return Some(format!(
            "Warning: {}",
            string(capture, "message").unwrap_or("debug capture failed")
        ));
    }

    event
        .as_object()?
        .values()
        .find_map(|body| string(body, "message"))
        .map(|message| format!("Warning: {message}"))
}

#[derive(Debug, clap::Args)]
struct MusicianArgs {
    /// Score to play until Ctrl-C or --duration.
    #[arg(value_name = "FILE", required = true)]
    file: Option<PathBuf>,
    /// Evaluate setup JavaScript on the same heap before the score.
    #[arg(long, value_name = "FILE", help_heading = "Playing")]
    prebake: Option<PathBuf>,
    /// Reload stable saves without restarting the Session or audio stream.
    #[arg(long, help_heading = "Playing")]
    watch: bool,
    /// Play for this many wall-clock seconds.
    #[arg(long, help_heading = "Playing")]
    duration: Option<f64>,
    /// Cycles per second the score starts at; setcps/setcpm in the score win.
    #[arg(long, default_value_t = 0.5, help_heading = "Playing")]
    cps: f64,
    /// Output buffer in frames - the latency knob: the callback size on
    /// macOS and Linux; on Windows (WASAPI shared mode) the callback stays
    /// at the 10 ms device period and this is the buffer queued ahead of it.
    /// Smaller plays a key press sooner and costs more CPU; automatic asks
    /// for 128 on real hardware. 32..=16384. RUSTEL_LIVE_BUFFER_FRAMES
    /// reaches the automatic path the same way.
    #[arg(
        long,
        value_name = "FRAMES",
        help_heading = "Playing",
        value_parser = clap::value_parser!(u32).range(
            i64::from(rustel_audio::AudioBufferPreference::MIN_FRAMES)
                ..=i64::from(rustel_audio::AudioBufferPreference::MAX_FRAMES),
        )
    )]
    buffer_frames: Option<u32>,
    #[command(flatten)]
    sample_access: SampleAccessArgs,
    /// How much of the set to record. Watched sets are recorded by default -
    /// `normal` keeps the saves that installed, `debug` also keeps rejected
    /// saves and engine diagnostics, which is what a bug report needs.
    #[arg(
        long,
        value_name = "MODE",
        num_args = 0..=1,
        default_missing_value = "debug",
        help_heading = "Session tapes"
    )]
    save_session: Option<SessionModeArg>,
    /// Do not record this set.
    ///
    /// Recording is on by default. A tape is a few kB of text, and a set that
    /// was not recorded cannot be recovered.
    #[arg(
        long,
        conflicts_with_all = ["save_session", "session_file"],
        help_heading = "Session tapes"
    )]
    no_save_session: bool,
    /// Where to write the recording.
    #[arg(
        long,
        value_name = "FILE",
        alias = "session-filename",
        help = product::SESSION_FILE_HELP,
        help_heading = "Session tapes"
    )]
    session_file: Option<PathBuf>,
    /// Print the active code to the terminal as it changes.
    ///
    /// Sugar for the same renderer as the `watch-code` subcommand.
    #[arg(long, long_help = product::FOLLOW_LONG_HELP, help_heading = "Playing")]
    follow: bool,
    /// Emit the bounded, versioned live-UI event stream on stdout.
    ///
    /// Stdout carries only the protocol. The run's own narration goes to
    /// stderr, as for any other command, and is JSON in this mode. The two
    /// streams carry different objects.
    #[arg(long, hide = true, conflicts_with = "follow")]
    ui_events: bool,
    /// Emit `score_active` JSON on stderr for `watch-code` or another editor.
    #[arg(long = "score-events", help_heading = "Playing")]
    announce_score: bool,
    /// Print the report as JSON instead of a readable summary.
    ///
    /// `--follow` draws the sounding score to the same stdout, which would
    /// leave a reader parsing prose out of the middle of a JSON document.
    #[arg(
        short = 'j',
        long,
        conflicts_with = "follow",
        help_heading = "Diagnostics"
    )]
    json: bool,
    /// Sounds to warm in addition to the ones the score itself names. Internal:
    /// a replay fills this from the whole tape, so a sample that first appears
    /// late in the set is ready before the set starts.
    #[arg(skip)]
    preload_sounds: Vec<String>,
    /// Start the first score a reload installs on its own first beat rather
    /// than the running count. Internal: set by replay from
    /// [`ReplayPlan::first_install_from_zero`].
    #[arg(skip)]
    first_install_from_zero: bool,
    /// Signalled once the watch loop anchors cycle zero. Internal: replay
    /// starts delivering saves only then.
    #[arg(skip)]
    anchored: Option<std::sync::mpsc::Sender<()>>,
    /// Publish a MIDI port of this name for other software to connect to.
    ///
    /// Saves setting up a loopback first: on macOS and Linux the DAW simply
    /// sees this name, with no IAC bus to enable. May be repeated. Windows has
    /// no such facility - install a loopback driver there and pass its port
    /// name to `.midi()` instead.
    ///
    /// A flag rather than something the score says, because which ports exist
    /// is a fact about this machine: the score stays `.midi('name')` and runs
    /// unchanged on strudel.cc.
    #[arg(
        long,
        value_name = "NAME",
        num_args = 0..=1,
        default_missing_value = product::COMMAND_NAME
    )]
    midi_virtual: Vec<String>,
    /// Send MIDI clock to this port, with Start, Continue and Stop.
    ///
    /// 24 pulses a beat and 96 a cycle, scheduled from the engine's own
    /// cycle-to-time mapping, so the hardware follows rustel rather than the
    /// other way round. A port named here rather than in the score, because
    /// which ports exist is a fact about this machine - as with
    /// `--midi-virtual`, the score stays portable to strudel.cc.
    #[arg(long, value_name = "PORT")]
    midi_clock_out: Option<String>,
    /// Follow MIDI clock from this port instead of the internal tempo.
    ///
    /// The scheduler bends its tempo towards the incoming clock and jumps
    /// when the phase is too far off to bend, so a `setcps` in the score is
    /// overridden while a clock is heard. Nothing starts playback by itself:
    /// an incoming Start is followed, not obeyed.
    #[arg(long, value_name = "PORT")]
    midi_clock_in: Option<String>,
    /// The audio input to open for `s("in")`: a device name, or a distinctive
    /// substring of one. `rustel devices` lists the inputs with their channel
    /// counts, which is how far `in:N` goes.
    ///
    /// Without it no input stream is opened at all and `s("in")` is silence -
    /// the studio has a device panel for this (^P, audio in), and the live
    /// command has no panel.
    #[arg(long, visible_alias = "input", value_name = "NAME")]
    audio_input: Option<String>,
}

/// What `replay --export` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ReplayExportFormat {
    /// Deterministic scalar PCM.
    Wav,
    /// The same render, LAME-encoded at 320 kbps.
    Mp3,
}

#[derive(clap::Args, Debug)]
struct ClearOptions {
    /// Delete without an interactive confirmation.
    #[arg(short = 'f', long)]
    force: bool,
    /// Describe what would be deleted without changing any files.
    #[arg(short = 'n', long)]
    dry_run: bool,
}

#[derive(Subcommand, Debug)]
enum SamplesCommand {
    /// Download the shipped sample packs onto disk, skipping what is there.
    ///
    /// The default library is what `s("bd")` and friends read: Dirt-Samples,
    /// the piano bank, VCSL, the General MIDI soundfonts and the rest of the
    /// pin file. Files land in the sample cache beside the configuration and
    /// the loader reads them from there first, so a cached set plays offline.
    /// A pack already on disk is skipped file by file, so a second run
    /// fetches only what is missing.
    ///
    /// A progress bar counts the files fetched and names the file that is
    /// being fetched. Interrupt with Ctrl-C at any point; what landed stays,
    /// and the next run continues from there.
    ///
    /// A pack's list is pinned and hash-verified, so what this caches is
    /// exactly what a session would fetch on demand. What a score imports
    /// itself - samples("https://..."), samples("local:...") - is its own
    /// request with its own grants (--allow-sample-origin,
    /// --allow-local-samples) and is fetched as the score plays; this
    /// command reaches the shipped packs.
    Cache {
        /// Only these packs, by name as --list shows them. With none, every
        /// pack in the pin file, in its order.
        #[arg(value_name = "PACK")]
        packs: Vec<String>,
        /// Print what each pack holds and how much of it is on disk, then
        /// exit. Fetches the pack lists, since a manifest's file count is
        /// the one thing that cannot be known without it.
        #[arg(long)]
        list: bool,
        /// Emit one JSON progress object per change on stderr and the
        /// summary as JSON on stdout, for scripting. No bar is drawn.
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Remove every downloaded sample file - the shipped packs' cache and
    /// the score-selected responses beside it - and say how much that was.
    ///
    /// Nothing decoded is touched: what a running session holds in RAM goes
    /// on playing, and the next sound that needs a file fetches it again.
    /// Files that are not the cache's own - anything in the folder that is
    /// not a digest-named entry - are left where they are. For the
    /// score-selected namespace only, pinned files kept, there is
    /// `clear-score-cache`.
    Clear {
        #[command(flatten)]
        options: ClearOptions,
        /// Print the report as JSON instead of a readable line.
        #[arg(short = 'j', long)]
        json: bool,
    },
}

impl SamplesCommand {
    /// Whether this subcommand speaks JSON, so the run's narration and its
    /// error envelope follow the same switch as its report.
    fn wants_json(&self) -> bool {
        match self {
            Self::Cache { json, .. } | Self::Clear { json, .. } => *json,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum SessionModeArg {
    /// Only the saves that installed: a clean, replayable performance.
    Normal,
    /// Every save plus engine diagnostics: the bug report.
    Debug,
}

impl From<SessionModeArg> for rustel_runtime::session_log::SessionMode {
    fn from(mode: SessionModeArg) -> Self {
        match mode {
            SessionModeArg::Normal => Self::Normal,
            SessionModeArg::Debug => Self::Debug,
        }
    }
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    /// Print the saved value, or its default when unset.
    Get {
        #[arg(value_enum)]
        key: ConfigKey,
    },
    /// Save a value for every Rustel command, including Studio.
    Set {
        #[arg(value_enum)]
        key: ConfigKey,
        #[arg(action = clap::ArgAction::Set)]
        value: bool,
    },
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum ConfigKey {
    /// Check for new stable releases during interactive use.
    #[value(name = "check_updates")]
    CheckUpdates,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Play a score through the audio device until Ctrl-C.
    ///
    /// `--watch` reloads each stable save with no restart of the audio
    /// stream. To write a file, use `rustel export`.
    Play(Box<MusicianArgs>),
    /// Read or write user-wide settings in rustel.json.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Remove score-selected network responses from the bounded disk cache.
    /// Pinned and host-trusted sample files are kept.
    ClearScoreCache {
        #[command(flatten)]
        options: ClearOptions,
    },
    /// Manage the downloaded sample library: fetch the shipped packs onto
    /// disk, or empty the cache.
    ///
    ///     rustel samples cache        # every pack, with a progress bar
    ///     rustel samples cache piano  # one pack, by name
    ///     rustel samples cache --list # what each pack holds, what is here
    ///     rustel samples clear        # remove every downloaded file
    Samples {
        #[command(subcommand)]
        command: SamplesCommand,
    },
    /// Serve a folder of samples to a browser, so strudel.cc (or any web
    /// build) can play them.
    ///
    #[command(long_about = product::SERVE_SAMPLES_LONG_ABOUT)]
    ServeSamples {
        /// Folder to serve (default: the working directory).
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Port to listen on. 5432 is what `samples('local:')` expects.
        #[arg(long, default_value_t = 5432)]
        port: u16,
        /// Address to bind. Localhost by default: `0.0.0.0` puts your sample
        /// folder on every network you are joined to.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Browser origin allowed to read the folder. Repeat to allow more
        /// than one. Supplying any value replaces the default
        /// `https://strudel.cc` grant.
        #[arg(long = "allow-origin", value_name = "ORIGIN")]
        allow_origins: Vec<String>,
    },
    /// Watch what a MIDI controller actually sends: notes, velocity, CC.
    ///
    ///     rustel midi-monitor MiniLab
    ///
    /// The panel legend on a controller and the CC numbers it emits routinely
    /// disagree, so the only reliable way to learn a knob is to turn it and
    /// look. Ends on Ctrl-C (or --duration) with a summary of every control
    /// seen and the range it covered - which is the mapping you then write into
    /// a score.
    MidiMonitor {
        /// Input port: one distinctive word of its name, or its index from
        /// `rustel devices`. Defaults to the first input.
        #[arg(value_name = "PORT")]
        port: Option<String>,
        /// Stop after this many seconds instead of waiting for Ctrl-C.
        #[arg(long, value_name = "SECS")]
        duration: Option<f64>,
        /// Print one JSON object per message, for piping.
        #[arg(long)]
        json: bool,
        /// Wait for ONE control to move, print the pattern that reads it, exit.
        /// Turn the knob you want to map.
        #[arg(long, conflicts_with_all = ["json", "quiet"])]
        learn: bool,
        /// Show clock and active-sensing bytes, which are otherwise hidden
        /// because a clocked device sends 24 of them per beat.
        #[arg(long)]
        timing: bool,
    },
    /// Watch what a gamepad actually sends, as a score reads it.
    ///
    ///     rustel gamepad-monitor
    ///
    /// The thing to run when a pad is plugged in and nothing moves: every
    /// button and stick prints as `gamepad(0) Controller: a pressed` or
    /// `x1 +0.72`, and anything no score can read prints with its raw code.
    GamepadMonitor {
        /// Stop after this many seconds; thirty by default.
        #[arg(long, value_name = "SECS")]
        duration: Option<f64>,
    },
    /// List every device this machine offers: MIDI in, MIDI out, and audio.
    ///
    /// The one command to run before writing a score that talks to hardware -
    /// `.midi()` needs an output name, a controller is an input (a different
    /// namespace, so the same box can appear in both), and the audio list says
    /// which device a set will actually open.
    Devices {
        /// Emit raw JSON instead of the human-readable list, for scripting.
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Report the binary, CPU, runtime tiers, selected capabilities, and audio facts.
    Doctor {
        /// Emit the stable versioned JSON schema instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Show the reference entry for a name: a function, control, chord,
    /// scale, sound, or painter.
    ///
    /// Chord and scale symbols resolve exactly as spelled - `doc ^7` and
    /// `doc +` find those chords - while other names match case-insensitively
    /// and by synonym, the same way the studio's reference panel looks them
    /// up. The text view is the very body the panel draws.
    #[cfg(feature = "studio")]
    Doc {
        /// The name to look up.
        #[arg(value_name = "NAME")]
        name: String,
        /// Emit the entry as JSON instead of readable text.
        #[arg(long)]
        json: bool,
    },
    #[command(long_about = product::WATCH_CODE_LONG_ABOUT)]
    WatchCode,
    /// List MIDI ports - outputs, then inputs - each in the order a numeric
    /// selector indexes them.
    ///
    /// Either name reaches an output: `.midi('IAC Driver Bus 1')` or
    /// `.midi(0)`; a score listens on an input with `midin('MiniLab')` or
    /// `midin(0)`.
    /// Names match case-insensitively on any substring, because the full names
    /// carry platform noise nobody should have to type.
    MidiList {
        /// Emit raw JSON instead of the human-readable list, for scripting.
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Replay a recorded set: the saves land on the same clock they were
    /// played on, through the real audio device.
    ///
    /// The score is written to a plain file as it goes, so pointing an editor
    /// at that file lets you WATCH the performance being typed.
    Replay {
        /// Emit the live event stream as JSON instead of sentences.
        ///
        /// A replay plays a score down the same path a watched one does, so
        /// it says the same things and takes the same flag to have them as
        /// objects. `-v` still chooses how much there is to say.
        #[arg(short = 'j', long)]
        json: bool,
        /// Session file written by `--save-session`.
        #[arg(value_name = "SESSION")]
        session: PathBuf,
        /// Also write the score to this path as it replays, so an editor
        /// pointed at it shows the set being typed. Off by default: replay
        /// then writes its working file to a temporary directory and removes
        /// it at the end.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Bounce the replayed set to WAV or MP3 instead of playing it: the
        /// recording becomes audio with no DAW and no loopback device.
        ///
        /// Renders offline in one pass, with no audio device. Saves install at
        /// their recorded offsets from `--from`; `--speed` does not apply.
        #[arg(long, value_name = "FILE")]
        export: Option<PathBuf>,
        /// Container for `--export`. Defaults to the extension of the path,
        /// so `--export set.mp3` needs no flag.
        #[arg(long, value_name = "FORMAT", value_enum)]
        format: Option<ReplayExportFormat>,
        /// Export this many seconds of the set, counted from `--from`.
        ///
        /// A session changes tempo whenever the artist did, so bars and cycles
        /// are not a length a tape can be trimmed by - export a single score
        /// with `--cycles` if you want a musical count.
        #[arg(long, value_name = "SECS", requires = "export")]
        duration: Option<f64>,
        /// Print the active code to the terminal as it changes, so you can
        /// watch the set play itself.
        ///
        /// This is the terminal renderer; use `--score-events` when another
        /// process or editor needs the JSON transport instead.
        #[arg(long)]
        follow: bool,
        /// Emit `score_active` JSON on stderr for `watch-code` or another editor.
        #[arg(long = "score-events", conflicts_with = "export")]
        score_events: bool,
        /// Compress the gaps between saves while leaving the music's tempo
        /// unchanged.
        ///
        /// Use it only for that. Each state gets proportionally fewer cycles,
        /// so alternations, `rib` sections and in-flight samples do not play
        /// as recorded. Reproduce bugs at 1, and use `--from` to reach a
        /// moment in the set.
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        /// Start this many seconds into the set - the same `t` the tape
        /// records against each save, counted from the moment the set began.
        /// `--from 600` opens ten minutes in. Not cycles, not bars.
        ///
        /// The state the set was sounding at that moment lands first, so the
        /// music starts where you asked rather than from the last edit.
        #[arg(long, default_value_t = 0.0)]
        from: f64,
        #[command(flatten)]
        sample_access: SampleAccessArgs,
    },
    /// Open the native modeless terminal studio.
    ///
    /// The score is edited and visualized in-process; F5 updates and plays,
    /// F8 stops, and Ctrl+S also updates. A missing `.strudel` file opens as a
    /// new score. Any other missing path opens as a new set folder.
    #[cfg(feature = "studio")]
    Studio {
        /// A set folder, or a score to open with its folder as the set.
        /// With none, the set open last time, or a new set in the sets
        /// folder on a first run.
        #[arg(value_name = "FILE")]
        file: Option<PathBuf>,
        /// Initial cycles per second; evaluated score settings may replace it.
        #[arg(long, default_value_t = 0.5)]
        cps: f64,
        /// Treat the whole editor buffer as mini-notation.
        #[arg(long)]
        mini: bool,
        /// Colour theme: a built-in name, a name under the user theme
        /// directory, or a path to a theme file. Defaults to $RUSTEL_THEME.
        #[arg(long, value_name = "NAME")]
        theme: Option<String>,
        /// Print the available theme names and the directory user themes are
        /// read from, then exit.
        #[arg(long)]
        list_themes: bool,
        /// Ask the hosting terminal what it can do - graphics protocols,
        /// cell size, glyph coverage - print the answers, and exit. The
        /// diagnostic for "why is the tier not engaging here".
        #[arg(long)]
        probe_terminal: bool,
        /// The audio output to open: a device name, or `silent` for none -
        /// the clock, the meters, the visualizers and takes without audio
        /// hardware (a machine without any, an ssh session, a recording).
        #[arg(long, value_name = "NAME")]
        output: Option<String>,
        /// Output buffer in frames - the latency knob: the callback size on
        /// macOS and Linux; on Windows (WASAPI shared mode) the callback
        /// stays at the 10 ms device period and this is the buffer queued
        /// ahead of it. Smaller plays a key press sooner and costs more CPU;
        /// automatic asks for 128 on real hardware. 32..=16384. The settings
        /// sheet's "audio out latency" row is the same knob, remembered.
        #[arg(
            long,
            value_name = "FRAMES",
            value_parser = clap::value_parser!(u32).range(
            i64::from(rustel_audio::AudioBufferPreference::MIN_FRAMES)
                ..=i64::from(rustel_audio::AudioBufferPreference::MAX_FRAMES),
        )
        )]
        buffer_frames: Option<u32>,
        /// The audio input to open for `s("in")`: a device name, or a
        /// distinctive substring of one. Without it no input stream is opened
        /// and `s("in")` is silence, so a score that listens needs this or a
        /// pick in the device panel (^P, audio in). `rustel devices` lists the
        /// inputs this build sees, with their channel counts - which is how
        /// far `in:N` goes.
        #[arg(long, value_name = "NAME")]
        input: Option<String>,
        #[command(flatten)]
        sample_access: SampleAccessArgs,
        /// How much of the set to record. Studio sets are recorded by default
        /// from the first evaluate - `normal` keeps the scores that
        /// installed, `debug` also keeps rejected ones.
        #[arg(long, value_name = "MODE", num_args = 0..=1, default_missing_value = "debug")]
        save_session: Option<SessionModeArg>,
        /// Do not record this set.
        #[arg(long, conflicts_with_all = ["save_session", "session_file"])]
        no_save_session: bool,
        /// Where to write the recording.
        #[arg(long, value_name = "FILE", help = product::SESSION_FILE_HELP)]
        session_file: Option<PathBuf>,
        /// Let remote clients press keys and read the screen.
        /// Defaults to 127.0.0.1:9247; pass a port or IP:port to change it.
        /// LAN and wildcard addresses are supported. This is plaintext TCP:
        /// use a trusted network or encrypted tunnel.
        /// Options > Remote control (F1, O, R) lets you enable it and copy
        /// the token. Send `auth <token>` once, then `key ctrl-s`, `key a`,
        /// or `screen`, one command per line. Clients have full UI control.
        #[cfg(feature = "remote-control")]
        #[arg(
            long,
            value_name = "ADDR",
            num_args = 0..=1,
            default_missing_value = "9247",
            require_equals = true,
            value_parser = rustel_studio::parse_bind
        )]
        remote_control: Option<std::net::SocketAddr>,
        /// Reuse this token across restarts and toggles. Otherwise each enable
        /// generates a fresh code and private token file. This option exposes
        /// the token in process arguments; avoid it on shared machines.
        #[cfg(feature = "remote-control")]
        #[arg(long, value_name = "TOKEN", requires = "remote_control", value_parser = rustel_studio::parse_auth_token)]
        auth_token: Option<String>,
        /// Not a studio flag. Accepted so the refusal can say where the
        /// studio's own prebakes live, rather than clap's "unexpected
        /// argument" sending someone to look for a file that is not the
        /// answer.
        #[arg(long, hide = true, value_name = "FILE")]
        prebake: Option<PathBuf>,
        /// Emit bounded Studio frame and input-to-paint measurements.
        #[arg(long, hide = true)]
        performance_events: bool,
    },
    /// Lint a score without playing it: syntax, mini-notation, and the names
    /// it uses - scales, notes, chords, and (once the sample manifests have
    /// loaded) sounds and banks. Reports findings and exits 1 for syntax or
    /// value errors; informational notes do not fail the check.
    ///
    /// Use `check` while editing to see every issue and where it is; use
    /// `validate` when a script only needs a yes/no "does this evaluate".
    Check {
        #[command(flatten)]
        input: SourceInput,
        /// Do not load the sample library; sound names are then not judged.
        #[arg(long)]
        no_samples: bool,
        /// Emit one JSON object per finding instead of the human-readable
        /// list, for scripting.
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Evaluate source and list the events it produces over a cycle span.
    ///
    /// One line an event by default; `--json` for the canonical haps, with
    /// exact fractions, that a script parses.
    Query {
        #[command(flatten)]
        input: SourceInput,
        /// Cycle begin (rational string like 0/1, or decimal).
        #[arg(long, default_value = "0")]
        begin: String,
        /// Cycle end (exclusive).
        #[arg(long, default_value = "1")]
        end: String,
        /// Print the report as JSON instead of a readable event list.
        #[arg(short, long)]
        json: bool,
    },
    /// Schedule a score for a finite duration and report what it played, or
    /// watch a file on the device sample clock.
    ///
    /// Nothing is heard unless `--device-audio` opens the system's default
    /// device: this is the headless harness that tests and scripts drive. To
    /// simply hear a score, use `rustel play <score>`, which opens a device
    /// for you.
    Trace {
        #[command(flatten)]
        input: SourceInput,
        /// Time to schedule: seconds (8, 8s), minutes (2m, 1:30), hours (1h),
        /// or bars (16b). Defaults to 2 seconds; incompatible with watch.
        #[arg(long)]
        duration: Option<String>,
        #[arg(long, default_value_t = 0.5)]
        cps: f64,
        /// Send scalar note/frequency output to default CPAL. With --watch,
        /// stream until stopped. Requires `--features device-audio`.
        #[arg(long)]
        device_audio: bool,
        /// Keep a source file running, installing stable saves on the live
        /// sample clock until SIGINT/SIGTERM. Requires --device-audio.
        #[arg(
            long,
            requires = "device_audio",
            conflicts_with_all = ["eval", "duration"]
        )]
        watch: bool,
        /// Print the report as JSON instead of a readable summary.
        #[arg(short = 'j', long)]
        json: bool,
        /// Emit `score_active` JSON on stderr for `watch-code` or another editor.
        #[arg(long = "score-events", requires = "watch")]
        score_events: bool,
    },
    /// Bounce a score to a file, offline - the same bytes every run.
    #[command(visible_alias = "export", long_about = product::RENDER_LONG_ABOUT)]
    Render {
        #[command(flatten)]
        input: SourceInput,
        /// Evaluate setup JavaScript on the same heap before the score.
        #[arg(long, value_name = "FILE")]
        prebake: Option<PathBuf>,
        /// How long: seconds (60, 30s), minutes (2m, 1:30) or bars (16b).
        /// Ends on the nearest whole cycle, never mid-bar. [default: 8 cycles]
        #[arg(long, value_name = "LENGTH", conflicts_with = "cycles")]
        duration: Option<String>,
        /// How many cycles to bounce - a musical length.
        #[arg(long, value_name = "N")]
        cycles: Option<f64>,
        /// After the length, keep going until the music has stayed quiet, so
        /// a reverb or delay tail is never cut; a looping score stops at the
        /// length plus a minute at most.
        #[arg(long)]
        until_silence: bool,
        /// What counts as quiet, in dBFS.
        #[arg(
            long,
            value_name = "DBFS",
            default_value_t = -60.0,
            allow_negative_numbers = true,
            requires = "until_silence"
        )]
        silence_floor: f64,
        /// How long it must stay quiet before the bounce ends, in seconds.
        #[arg(
            long,
            value_name = "SECS",
            default_value_t = 2.0,
            requires = "until_silence"
        )]
        silence_hold: f64,
        /// Cycles per second the score starts at; setcps/setcpm in the score win.
        #[arg(long, default_value_t = 0.5)]
        cps: f64,
        /// Where to write. [default: the score's name with the format's
        /// extension, beside the score]
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// The file's format; read off --output's extension when absent
        /// (.mp3 is mp3, .json is onset-json, anything else the normal WAV).
        #[arg(long, value_enum)]
        format: Option<RenderCliFormat>,
        /// The sample rate in Hz.
        #[arg(long, default_value_t = 48000, value_name = "HZ")]
        sample_rate: u32,
        /// Print the report as JSON - only JSON - instead of a line of text.
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Microbench query throughput; prints JSON metrics.
    Bench {
        #[command(flatten)]
        input: SourceInput,
        #[arg(long, default_value = "0")]
        begin: String,
        #[arg(long, default_value = "1")]
        end: String,
        #[arg(long, default_value_t = 200)]
        iterations: u64,
        /// Print the metrics as JSON instead of a readable summary.
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Evaluate a score exactly once and report yes/no: does it evaluate?
    ///
    /// A single verdict for scripts - exit 0 and "valid" when the score
    /// evaluates, non-zero otherwise. For a list of every issue and where it
    /// is, use `check` instead.
    Validate {
        #[command(flatten)]
        input: SourceInput,
        /// Emit the verdict as JSON instead of a human-readable line.
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Tab completion for every command, flag and value.
    ///
    ///     rustel completions --install        # your $SHELL, into its completions folder
    ///     rustel completions zsh --install    # or name the shell
    ///     rustel completions fish             # or print the script
    ///
    /// --install writes the script where the shell looks on its own (zsh's
    /// ~/.zfunc, bash-completion's user folder, fish's completions folder,
    /// elvish's lib, a PowerShell profile script) and prints the one rc line
    /// the shell still needs, if any; it never edits an rc file.
    Completions {
        /// The shell; read from $SHELL when left out.
        #[arg(value_enum)]
        shell: Option<clap_complete::Shell>,
        /// Write the script where the shell looks for completions instead of
        /// printing it, and say what, if anything, the shell's rc still needs.
        #[arg(long)]
        install: bool,
    },
}

#[derive(Clone, Debug, clap::Args)]
struct SourceInput {
    /// Path to a JS score source file.
    file: Option<PathBuf>,
    /// Inline score expression: JavaScript, with mini-notation inside quotes.
    #[arg(short = 'e', long = "eval")]
    eval: Option<String>,
    #[command(flatten)]
    sample_access: SampleAccessArgs,
}

#[derive(Clone, Debug, Default, clap::Args)]
struct SampleAccessArgs {
    /// Permit score-level samples() requests to this exact HTTP(S) origin.
    /// May be repeated. Redirects to other origins remain blocked.
    #[arg(
        long = "allow-sample-origin",
        value_name = "ORIGIN",
        help_heading = "Sample access"
    )]
    origins: Vec<String>,
    /// Fetch score samples() only from origins granted above. By default a
    /// public https origin whose server consents via CORS
    /// (`Access-Control-Allow-Origin: *`) is fetched without a grant - the
    /// same set strudel.cc can reach. Private and loopback addresses are
    /// refused either way.
    #[arg(long = "strict-sample-origins", help_heading = "Sample access")]
    strict_origins: bool,
    /// Permit samples('local:...') beneath this directory.
    #[arg(long = "allow-local-samples", value_name = "DIR")]
    local_root: Option<PathBuf>,
    /// Permit score-chosen OSC destinations other than loopback.
    /// Repeatable. The value must be an IP literal; names are not resolved
    /// on the music thread.
    #[cfg(feature = "osc")]
    #[arg(long = "allow-osc-host", value_name = "IP")]
    osc_hosts: Vec<String>,
}

fn with_sample_access(
    mut config: SessionConfig,
    args: &SampleAccessArgs,
) -> Result<SessionConfig, RuntimeError> {
    let mut access = ScoreSampleAccess::denied();
    if !args.strict_origins {
        access.permit_public_cors_origins();
    }
    for origin in &args.origins {
        access
            .permit_origin(origin)
            .map_err(RuntimeError::Message)?;
    }
    if let Some(root) = &args.local_root {
        access
            .permit_local_root(root)
            .map_err(RuntimeError::Message)?;
    }
    config.score_sample_access = access;
    #[cfg(feature = "osc")]
    {
        let mut osc_access = ScoreOscAccess::loopback_only();
        for host in &args.osc_hosts {
            osc_access
                .permit_host(host)
                .map_err(RuntimeError::Message)?;
        }
        config.score_osc_access = osc_access;
    }
    Ok(config)
}

/// Score and optional prebake loaded before live playback.
/// Keep both together so the live producer receives the explicit prebake.
struct LoadedLiveSources {
    score: String,
    prebake: Option<(PathBuf, String)>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RenderCliFormat {
    Wav,
    ScalarWav,
    /// The scalar render as 32-bit float WAV, unclamped. For comparing
    /// against a browser, whose OfflineAudioContext returns unclamped floats;
    /// a normal bounce should stay `scalar-wav`, which is what strudel.cc
    /// itself writes.
    ScalarF32,
    OnsetJson,
    /// The scalar render LAME-encoded at 320 kbps.
    Mp3,
}

impl From<RenderCliFormat> for RenderFormat {
    fn from(value: RenderCliFormat) -> Self {
        match value {
            RenderCliFormat::Wav => RenderFormat::Wav,
            RenderCliFormat::ScalarWav => RenderFormat::ScalarWav,
            RenderCliFormat::ScalarF32 => RenderFormat::ScalarF32Wav,
            RenderCliFormat::OnsetJson => RenderFormat::OnsetJson,
            RenderCliFormat::Mp3 => RenderFormat::ScalarMp3,
        }
    }
}

/// Exit codes, as a contract rather than an accident.
///
/// A caller can only react to a failure it can identify, and "non-zero" is not
/// an identification. `2` is left to clap for usage errors, and `128 + signal`
/// is the shell convention for a signalled exit.
mod exit {
    /// The source, the pattern or the environment was wrong.
    pub const FAILURE: u8 = 1;
    /// A limit refused the work: shorter window, thinner pattern, retry.
    pub const RESOURCE_LIMIT: u8 = 3;
    /// The filesystem said no.
    pub const IO: u8 = 4;
    /// Native audio host/device discovery or playback failed.
    pub const AUDIO: u8 = 5;
}

/// The signal that requested shutdown, or zero before any signal arrives.
///
/// The Unix handler only updates atomics. Allocation, locking, and I/O
/// must remain outside the signal handler.
static INTERRUPTED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// Stop signal count. The first stops the set and lets its tail ring out;
/// a second skips the remaining wait.
#[cfg(any(unix, windows, feature = "device-audio"))]
static INTERRUPT_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// Set when the run has finished and the process is free to end.
///
/// Read only by Windows' console handler for the CLOSING events, where the
/// process dies the moment the handler returns and the only way to keep a
/// shutdown is to stand in front of it.
#[cfg(windows)]
static SHUTDOWN_SETTLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Same signal as a boolean for bounded evaluators whose interrupt callback
/// must be able to observe cancellation without depending on CLI globals.
static EVALUATION_CANCELLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Read only by the ring-out below, which the device build owns.
#[cfg(feature = "device-audio")]
fn interrupt_count() -> u32 {
    INTERRUPT_COUNT.load(std::sync::atomic::Ordering::SeqCst)
}

/// Let a stopped mix fall quiet before closing the stream.
///
/// The tail wait ends on sustained silence, another stop signal, or its cap.
/// A separate bounded wait lets already-rendered audio reach the device.
#[cfg(feature = "device-audio")]
fn drain_tail_after_stop(device: &rustel_audio::LiveScalarDevice, output: LiveOutput) {
    const FLOOR: f32 = 1e-4;
    const HOLD: std::time::Duration = std::time::Duration::from_millis(120);
    const CAP: std::time::Duration = std::time::Duration::from_secs(8);
    let began = interrupt_count();
    // Explain the drain before waiting and advertise how to interrupt it.
    // Keep this at default verbosity so shutdown does not look stalled.
    output.event(
        LiveDetail::Essential,
        serde_json::json!({
            "stopping": {
                "message": "letting the tail ring out - Ctrl-C again to quit now",
            }
        }),
    );
    // The tap is off during ordinary playback so the callback does no
    // per-sample atomic work; shutdown is exactly when that cost is free.
    device.set_analysis_enabled(true);
    let started = std::time::Instant::now();
    let mut quiet_since: Option<std::time::Instant> = None;
    let mut window = [0.0f32; rustel_audio::LIVE_ANALYSIS_WINDOW_SAMPLES];
    let mut stopped_early = false;
    while started.elapsed() < CAP {
        if interrupt_count() > began {
            stopped_early = true;
            break;
        }
        if device.copy_analysis_window(&mut window).is_some() {
            let peak = window.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
            if peak < FLOOR {
                match quiet_since {
                    Some(since) if since.elapsed() >= HOLD => break,
                    Some(_) => {}
                    None => quiet_since = Some(std::time::Instant::now()),
                }
            } else {
                quiet_since = None;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // The analysis tap observes rendered frames ahead of device playback.
    // Let queued audio finish before closing the stream, unless another
    // stop signal skips this wait.
    let mut flushed_ms = 0u128;
    if !stopped_early {
        // Re-read latency because the device buffer can deepen during shutdown.
        let flush_started = std::time::Instant::now();
        loop {
            let target = (std::time::Duration::from_nanos(device.report().playback_latency_nanos)
                + std::time::Duration::from_millis(20))
            .min(std::time::Duration::from_millis(500));
            if flush_started.elapsed() >= target || interrupt_count() > began {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        flushed_ms = flush_started.elapsed().as_millis();
    }
    device.set_analysis_enabled(false);
    output.event(
        LiveDetail::Verbose,
        serde_json::json!({
            "stopped": {
                "tail_ms": started.elapsed().as_millis(),
                "flushed_ms": flushed_ms,
                "cut_short": stopped_early,
                "message": if stopped_early {
                    "stopped now"
                } else {
                    "let the tail ring out; press Ctrl-C again to quit sooner"
                },
            }
        }),
    );
}

fn interrupted_by() -> Option<i32> {
    match INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

#[cfg(unix)]
extern "C" fn on_signal(signal: i32) {
    // Async-signal-safe: atomic operations only; no allocation, lock, or I/O.
    INTERRUPTED.store(signal, std::sync::atomic::Ordering::SeqCst);
    INTERRUPT_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    EVALUATION_CANCELLED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Record SIGINT, SIGTERM and SIGHUP instead of letting them end the process.
///
/// The default disposition terminates the process immediately, so the
/// scheduler does not stop cleanly. A recorded signal lets the ordinary
/// cancellation path run: the same `Transport::stop` a library caller uses.
///
/// SIGHUP is included because closing the terminal window sends it. Without
/// a handler, a closed window can end the studio between two debounced
/// writes and lose the scene that is being typed.
///
/// Unix only. Windows has console control events instead; the handler below
/// records them.
#[cfg(unix)]
fn install_signal_handlers() {
    // SAFETY: `on_signal` is async-signal-safe (only atomic operations), and
    // `signal` is being used exactly as intended.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_signal as *const () as libc::sighandler_t);
    }
}

/// Which stop a Windows console event is, or `None` for one that is not ours.
///
/// Windows numbers nothing, so the Unix numbers are borrowed: a caller
/// reading the exit code sees the 130 it already knows for an interrupt, 129
/// for a closed window, 143 for a logoff or shutdown.
#[cfg(windows)]
fn console_event_signal(event: u32) -> Option<i32> {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };
    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => Some(SIGINT),
        CTRL_CLOSE_EVENT => Some(SIGHUP),
        CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => Some(SIGTERM),
        _ => None,
    }
}

/// Whether a console event ends the process the moment the handler returns.
///
/// The two interrupts do not: they are a request, and a handler that answers
/// TRUE leaves the process running. The three closing ones do, which is why
/// they are the only ones that wait.
#[cfg(windows)]
fn console_event_closes(event: u32) -> bool {
    use windows_sys::Win32::System::Console::{
        CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };
    matches!(
        event,
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
    )
}

/// Install the Windows console control handler, as the Unix handler records signals.
///
/// Windows delivers these events on a thread that it creates. The handler
/// makes the same atomic stores as the Unix handler, so the transport
/// watcher, the tail drain and the `128 + signal` exit need no platform code.
/// Without a handler, the default one ends the process on the first Ctrl-C:
/// the scheduler does not stop and `drain_tail_after_stop` does not run.
///
/// ```text
/// CTRL_C, CTRL_BREAK            record -> return TRUE -> process continues
/// CTRL_CLOSE, LOGOFF, SHUTDOWN  record -> wait for SHUTDOWN_SETTLED or
///                               CLOSE_GRACE -> return TRUE -> process ends
/// ```
///
/// An interrupt is a request: after TRUE the default handler does not run, so
/// the first stop lets the tail ring out and a second one skips the wait
/// through `INTERRUPT_COUNT`. A closing event is not a request: Windows ends
/// the process when the handler returns or when its own time limit expires.
#[cfg(windows)]
fn install_signal_handlers() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    /// Shorter than the five seconds Windows is documented to allow, because
    /// that allowance is a registry value somebody may have lowered and being
    /// killed halfway through a shutdown is worse than cutting it short.
    const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(2_500);

    unsafe extern "system" fn on_console_event(event: u32) -> windows_sys::core::BOOL {
        let Some(signal) = console_event_signal(event) else {
            // Not ours: let the next handler have it.
            return 0;
        };
        // The same atomic updates the Unix handler makes, in the same order.
        INTERRUPTED.store(signal, std::sync::atomic::Ordering::SeqCst);
        INTERRUPT_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        EVALUATION_CANCELLED.store(true, std::sync::atomic::Ordering::SeqCst);
        if console_event_closes(event) {
            // Unlike the Unix handler this one may block: it is the OS's
            // thread, not ours, and returning from it is what kills us.
            let until = std::time::Instant::now() + CLOSE_GRACE;
            while !SHUTDOWN_SETTLED.load(std::sync::atomic::Ordering::SeqCst)
                && std::time::Instant::now() < until
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        1
    }

    // A failure here is not worth ending a run over: it means this process
    // has no console (a service, a detached GUI launch), where there is no
    // Ctrl-C to receive in the first place.
    let _ = unsafe { SetConsoleCtrlHandler(Some(on_console_event), 1) };
}

#[cfg(not(any(unix, windows)))]
fn install_signal_handlers() {}

/// Watch for a recorded signal and stop the transport when one arrives.
///
/// The handler cannot do this itself, so a thread polls. Returns a guard that
/// ends the thread; polling at 10ms bounds the delay between the signal and the
/// stop without spinning.
fn watch_for_interrupt(transport: std::sync::Arc<rustel_scheduler::Transport>) -> InterruptWatcher {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    let handle = std::thread::spawn(move || {
        while !flag.load(std::sync::atomic::Ordering::SeqCst) {
            // Reapply the stop each poll: `play` can call `transport.start()`
            // after an early signal, clearing an earlier stop.
            if interrupted_by().is_some() {
                transport.stop();
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
    InterruptWatcher {
        done,
        handle: Some(handle),
    }
}

struct InterruptWatcher {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for InterruptWatcher {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::SeqCst);
        // Join so the watcher releases its transport before this guard is dropped.
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn exit_code_for(kind: &str) -> u8 {
    match kind {
        "resource-limit" => exit::RESOURCE_LIMIT,
        "io" => exit::IO,
        "audio" => exit::AUDIO,
        // Handled in `main`, which maps it to 128 + signal so an interrupted
        // run is distinguishable from a failed one.
        "interrupted" => exit::FAILURE,
        _ => exit::FAILURE,
    }
}

fn main() -> ExitCode {
    // This runs before any thread exists and for every command. Memory
    // that the sample loaders free then returns to the system immediately.
    rustel_runtime::free_memory::fix_release_thresholds();
    install_signal_handlers();
    // A contained score panic is reported as a `panic` error by the command
    // that caught it, so it does not also print over the output.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !rustel_runtime::panic_is_contained() {
            default_hook(info);
        }
    }));
    // The command line reports recoverable notices and progress on stderr;
    // the library collects them unless its host asks for this.
    rustel_voice::set_default_direct_diagnostic_logging(true);
    // All work runs on a thread with an explicit stack. Pattern queries
    // recurse once per graph node, and a legitimately deep native graph
    // (which the recursion bound admits) overflows the platform default
    // main-thread stack under debug-profile frame sizes. An explicit stack
    // gives every platform and build profile the same headroom.
    let worker = std::thread::Builder::new()
        .name("rustel".into())
        .stack_size(rustel_runtime::QUERY_WORKER_STACK_BYTES)
        .spawn(run)
        .expect("spawn the rustel worker thread");
    // On macOS the gamepads are read through a framework that delivers
    // only through the main thread's run loop, so the main thread, which
    // would otherwise sit in the join, pumps it until the work is done.
    #[cfg(feature = "gamepad")]
    if rustel_runtime::gamepad::needs_the_main_thread() {
        while !worker.is_finished() {
            rustel_runtime::gamepad::pump();
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
    }
    let outcome = worker
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    // The run is over; a console handler standing in front of the process
    // exit on our behalf can stand down.
    #[cfg(windows)]
    SHUTDOWN_SETTLED.store(true, std::sync::atomic::Ordering::SeqCst);
    match outcome {
        Ok(()) => {
            // A run that completed AFTER a signal still exited because of it.
            match interrupted_by() {
                Some(signal) => ExitCode::from(128u8.saturating_add(signal as u8)),
                None => ExitCode::SUCCESS,
            }
        }
        // An interrupt is not a failure to report as one: same 128 + signal
        // exit as a run that was signalled mid-flight, so a caller sees one
        // outcome for "the user stopped it" however early it arrived.
        Err(RuntimeError::Interrupted(signal)) => {
            ExitCode::from(128u8.saturating_add(signal as u8))
        }
        // Stopped part way. Reported as the SAME outcome as an interrupt that
        // arrived before the work started: from the caller's side both are "the
        // user stopped it", and the signal number is what distinguishes them.
        Err(RuntimeError::Cancelled) => match interrupted_by() {
            Some(signal) => ExitCode::from(128u8.saturating_add(signal as u8)),
            None => ExitCode::from(exit::FAILURE),
        },
        Err(err) => {
            // A bounded evaluator may surface its own error immediately after
            // a signal arrived. The user's stop is still the controlling
            // outcome; do not relabel it as evaluation/resource failure merely
            // because that boundary won the final instruction race.
            if let Some(signal) = interrupted_by() {
                return ExitCode::from(128u8.saturating_add(signal as u8));
            }
            let kind = err.kind();
            // Errors go to stderr; stdout stays reserved for the command's own
            // output. Text for a person unless the command was asked for
            // JSON, in which case the one-line envelope a script can parse -
            // never a mix, whichever stream is a pipe.
            let message = err.to_string();
            if json_asked() {
                let envelope = serde_json::json!({
                    "error": {
                        "kind": kind,
                        "message": message,
                    }
                });
                eprintln!("{envelope}");
            } else {
                // The kind is the label; a message that repeats it says it once.
                let message = message
                    .strip_prefix(&format!("{kind}: "))
                    .unwrap_or(&message);
                let on = style::stderr_on();
                let message = style::safe_source(message);
                eprintln!("{}", style::red(on, &format!("✗ {kind}: {message}")));
            }
            ExitCode::from(exit_code_for(kind))
        }
    }
}

fn run() -> Result<(), RuntimeError> {
    // Apply display flags before clap renders help or a parse error.
    let args: Vec<_> = std::env::args_os().collect();
    NO_COLOR_MODE.store(
        args.iter()
            .skip(1)
            .take_while(|arg| *arg != "--")
            .any(|arg| arg == "--no-color" || arg == "--plain"),
        std::sync::atomic::Ordering::Relaxed,
    );
    if args.len() == 1 {
        Cli::command()
            .print_help()
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        println!();
        return Ok(());
    }
    let cli = Cli::try_parse_from(&args).unwrap_or_else(|mut error| {
        if let Some(tip) = play_tip(&error) {
            error.insert(
                clap::error::ContextKind::Suggested,
                clap::error::ContextValue::StyledStrs(vec![tip.into()]),
            );
        }
        error.exit()
    });
    QUIET_MODE.store(cli.quiet, std::sync::atomic::Ordering::Relaxed);
    NO_INPUT_MODE.store(cli.no_input, std::sync::atomic::Ordering::Relaxed);
    NO_COLOR_MODE.store(
        cli.no_color || cli.plain,
        std::sync::atomic::Ordering::Relaxed,
    );
    // One switch for the whole run, read once from the parsed line.
    let json = cli.wants_json();
    JSON_MODE.store(json, std::sync::atomic::Ordering::Relaxed);
    rustel_runtime::set_progress_json(json);
    let verbosity = cli.verbose;
    let dispatch = cli.acceleration.dispatch();
    let check_updates = cli.checks_for_updates(
        std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && std::io::stderr().is_terminal(),
        std::env::var_os("CI").is_some_and(|value| !value.is_empty()),
    );
    run_command(cli.command, verbosity, dispatch, check_updates)
}

// The test-only observer reads the constructed config before it is returned.
#[allow(clippy::let_and_return)]
fn session_config(dispatch: rustel_audio::DspDispatch) -> SessionConfig {
    let config = SessionConfig::default().with_dsp_dispatch(dispatch);
    #[cfg(test)]
    dsp_selection_tests::BUILT_DISPATCH.set(Some(config.dsp_dispatch));
    config
}

#[cfg(feature = "device-audio")]
fn live_output_options(
    session: &Session,
    buffer_frames: Option<u32>,
) -> rustel_audio::LiveOutputOptions {
    let options =
        rustel_audio::LiveOutputOptions::default().with_dispatch(session.config().dsp_dispatch);
    match buffer_frames {
        Some(frames) => {
            options.with_buffer_preference(rustel_audio::AudioBufferPreference::Frames(frames))
        }
        None => options,
    }
}

/// Keep the live callback on the accepted Session setting without reopening it.
#[cfg(feature = "device-audio")]
fn sync_live_polyphony(session: &Session, device: &rustel_audio::LiveScalarDevice) {
    device.set_max_polyphony(session.max_polyphony());
}

fn run_command(
    command: Command,
    verbosity: u8,
    dispatch: rustel_audio::DspDispatch,
    check_updates: bool,
) -> Result<(), RuntimeError> {
    if check_updates {
        match &command {
            #[cfg(feature = "studio")]
            Command::Studio { .. } => {}
            _ => rustel_runtime::updates::check_for_updates(|message| eprintln!("{message}")),
        }
    }
    match command {
        Command::Play(musician) => run_musician(*musician, verbosity, dispatch),
        Command::Config { command } => {
            match command {
                ConfigCommand::Get {
                    key: ConfigKey::CheckUpdates,
                } => println!("{}", rustel_runtime::settings::check_updates()?),
                ConfigCommand::Set {
                    key: ConfigKey::CheckUpdates,
                    value,
                } => rustel_runtime::settings::set_check_updates(value)?,
            }
            Ok(())
        }
        #[cfg(feature = "studio")]
        Command::Studio {
            file,
            cps,
            mini,
            theme,
            list_themes,
            probe_terminal,
            output,
            buffer_frames,
            input,
            sample_access,
            save_session,
            no_save_session,
            session_file,
            prebake,
            performance_events,
            #[cfg(feature = "remote-control")]
            remote_control,
            #[cfg(feature = "remote-control")]
            auth_token,
        } => {
            if prebake.is_some() {
                return Err(RuntimeError::Message(
                    "the studio keeps its own prebakes: open the settings sheet (^O) and \
                     press Enter on \"global prebake\" or \"local prebake\". The global one is \
                     a file beside studio.json, the local one lives in the set's \
                     rustel-set.json. --prebake belongs to `rustel play <score>`"
                        .into(),
                ));
            }
            if probe_terminal {
                let features = rustel_studio::terminal::probe_terminal();
                println!(
                    "{}",
                    serde_json::json!({
                        "terminal": features.name,
                        "truecolor": features.truecolor,
                        "kitty_graphics": features.kitty_graphics,
                        "sixel": features.sixel,
                        "fine_glyphs": features.fine_glyphs,
                        "sync_output": features.sync_output,
                        "cell_pixels": features.cell_pixels,
                        "pixel_mouse": features.pixel_mouse,
                        "tier": features.default_tier().label(),
                    })
                );
                return Ok(());
            }
            if list_themes {
                println!(
                    "{}",
                    serde_json::json!({
                        "themes": rustel_studio::theme::Theme::available_names(),
                        "directory": rustel_studio::theme::theme_directory()
                            .map(|path| path.display().to_string()),
                    })
                );
                return Ok(());
            }
            let cps = parse_cps(cps)?;
            let session =
                with_sample_access(session_config(dispatch).with_cps(cps), &sample_access)?;
            let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let _interrupt_watcher = watch_for_studio_interrupt(cancellation.clone());
            let recording = (!no_save_session).then(|| rustel_studio::RecordingOptions {
                mode: save_session.unwrap_or(SessionModeArg::Normal).into(),
                file: session_file,
            });
            rustel_studio::run(rustel_studio::StudioOptions {
                path: file,
                mini,
                session,
                theme,
                output,
                output_buffer_frames: buffer_frames,
                input,
                cancellation: Some(cancellation),
                recording,
                build_features: BUILD_FEATURES,
                performance_events,
                check_updates,
                #[cfg(feature = "remote-control")]
                remote_control,
                #[cfg(feature = "remote-control")]
                auth_token,
                ..rustel_studio::StudioOptions::new(PathBuf::new())
            })
        }
        Command::ClearScoreCache { options } => {
            let path = rustel_runtime::score_sample_cache_dir();
            let status = if options.dry_run {
                "dry_run"
            } else {
                confirm_cache_clear(&path, &options)?;
                let empty = directory_sizes(&path).is_empty();
                rustel_runtime::clear_score_sample_cache().map_err(RuntimeError::Message)?;
                if empty { "empty" } else { "cleared" }
            };
            println!(
                "{}",
                serde_json::json!({
                    "score_sample_cache": {
                        "status": status,
                        "scope": "score-selected responses; pinned and host-trusted files are kept",
                        "path": path.display().to_string(),
                    }
                })
            );
            Ok(())
        }
        Command::Samples { command } => match command {
            SamplesCommand::Cache { packs, list, json } => run_samples_cache(&packs, list, json),
            SamplesCommand::Clear { json, options } => run_samples_clear(json, &options),
        },
        Command::WatchCode => run_watch_code(),
        Command::MidiMonitor {
            port,
            duration,
            json,
            learn,
            timing,
        } => run_midi_monitor(
            port.as_deref(),
            duration,
            json,
            quiet_asked(),
            learn,
            timing,
        ),
        Command::GamepadMonitor { duration } => run_gamepad_monitor(duration),
        Command::Devices { json } => run_devices(json),
        Command::Doctor { json } => run_doctor(json, dispatch),
        #[cfg(feature = "studio")]
        Command::Doc { name, json } => run_doc(&name, json),
        Command::MidiList { json } => run_midi_list(json),
        Command::Completions { shell, install } => {
            let shell = match shell.or_else(shell_from_environment) {
                Some(shell) => shell,
                None => {
                    return Err(RuntimeError::Message(
                        "say which shell: completions bash | zsh | fish | elvish | powershell"
                            .into(),
                    ));
                }
            };
            if install {
                install_completions(shell)
            } else {
                run_completions(shell);
                Ok(())
            }
        }
        Command::ServeSamples {
            dir,
            port,
            host,
            allow_origins,
        } => {
            let dir = dir.unwrap_or_else(|| PathBuf::from("."));
            rustel_runtime::sample_server::serve_until_with_origins(
                &dir,
                &host,
                port,
                &allow_origins,
                &|| interrupted_by().is_some(),
            )
            .map_err(RuntimeError::Message)
        }
        Command::Replay {
            // Read through the JSON mode the whole run shares, the way
            // every other command's is.
            json: _,
            session,
            out,
            speed,
            from,
            export,
            format,
            duration,
            follow,
            score_events,
            sample_access,
        } => run_replay(
            &session,
            ReplayRun {
                out,
                speed,
                from,
                export,
                export_format: format,
                duration,
                follow,
                score_events,
                sample_access,
            },
            verbosity,
            dispatch,
        ),
        Command::Check {
            input,
            no_samples,
            json,
        } => {
            let source = read_source(&input)?;
            let mut session = Session::with_config(with_sample_access(
                session_config(dispatch),
                &input.sample_access,
            )?)?;
            if !rustel_runtime::sounds::samples_imports(&source).is_empty() {
                session.set_direct_diagnostic_logging(false);
            }
            let library = if no_samples {
                None
            } else {
                if session.enable_default_samples().is_ok() {
                    session.wait_for_sample_loads(std::time::Duration::from_secs(5));
                }
                session.sample_library().cloned()
            };
            // A score is JavaScript with mini-notation inside quotes, so
            // `check_score` lints it with `mini` false.
            //
            // The static lint does not run the score. It cannot see a name
            // that fails to resolve only at evaluation, or a score that
            // throws. The dry evaluation in `check_score` catches those,
            // with the mini-notation compatibility fallback disabled.
            let mut check = rustel_runtime::score_check::check_score(
                &source,
                library.as_deref(),
                &mut session,
                &EVALUATION_CANCELLED,
            );
            let failures = sample_import_failures(&source, &mut session);
            check.diagnostics.extend(failures.placed);
            if !failures.unplaced.is_empty() {
                check
                    .eval_error
                    .get_or_insert(rustel_runtime::score_check::EvalError {
                        message: failures.unplaced.join("; "),
                        line: None,
                    });
            }
            let diagnostics = &check.diagnostics;
            let problems = diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.level != rustel_runtime::lint::Level::Note)
                .count();
            let has_issues = problems > 0 || check.eval_error.is_some();
            let level_name = |level: rustel_runtime::lint::Level| match level {
                rustel_runtime::lint::Level::Syntax => "syntax",
                rustel_runtime::lint::Level::Value => "value",
                rustel_runtime::lint::Level::Note => "note",
            };
            let line_of = |from: usize| {
                source[..from.min(source.len())]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1
            };
            if json {
                for diagnostic in diagnostics {
                    println!(
                        "{}",
                        serde_json::json!({
                            "check": {
                                "level": level_name(diagnostic.level),
                                "line": line_of(diagnostic.from),
                                "from": diagnostic.from,
                                "to": diagnostic.to,
                                "message": diagnostic.message,
                            }
                        })
                    );
                }
                if let Some(eval_error) = &check.eval_error {
                    println!(
                        "{}",
                        serde_json::json!({
                            "check": {
                                "level": "error",
                                "line": eval_error.line,
                                "message": eval_error.message,
                            }
                        })
                    );
                }
                if !has_issues {
                    println!("{}", serde_json::json!({ "check": { "status": "ok" } }));
                }
            } else {
                let on = style::stdout_on();
                if !has_issues {
                    println!("{}", style::green(on, "✓ no issues found"));
                } else {
                    for diagnostic in diagnostics {
                        let badge = match diagnostic.level {
                            rustel_runtime::lint::Level::Syntax => {
                                style::red(on, level_name(diagnostic.level))
                            }
                            rustel_runtime::lint::Level::Value => {
                                style::yellow(on, level_name(diagnostic.level))
                            }
                            rustel_runtime::lint::Level::Note => {
                                style::cyan(on, level_name(diagnostic.level))
                            }
                        };
                        println!(
                            "{} {} {}",
                            badge,
                            style::dim(on, &format!("line {}", line_of(diagnostic.from))),
                            style::safe_source(&diagnostic.message)
                        );
                    }
                    if let Some(eval_error) = &check.eval_error {
                        let badge = style::red(on, "error");
                        let message = style::safe_source(&eval_error.message);
                        match eval_error.line {
                            Some(line) => println!(
                                "{} {} {}",
                                badge,
                                style::dim(on, &format!("line {line}")),
                                message
                            ),
                            None => println!("{} {}", badge, message),
                        }
                    }
                    if has_issues {
                        let total = problems + usize::from(check.eval_error.is_some());
                        println!("{}", style::red(on, &format!("{total} issue(s) found")));
                    } else {
                        println!(
                            "{}",
                            style::cyan(
                                on,
                                &format!("{} note(s); no issues found", diagnostics.len())
                            )
                        );
                    }
                }
            }
            if has_issues {
                Err(RuntimeError::Message(
                    "check found issues; see findings on stdout".into(),
                ))
            } else {
                Ok(())
            }
        }
        Command::Query {
            input,
            begin,
            end,
            json,
        } => {
            let mut session = Session::with_config(with_sample_access(
                session_config(dispatch),
                &input.sample_access,
            )?)?;
            let _watcher = watch_for_interrupt(session.transport());
            let source = read_source(&input)?;
            if !rustel_runtime::sounds::samples_imports(&source).is_empty() {
                session.set_direct_diagnostic_logging(false);
            }
            // Check sound names only in `check`, which has the sample library.
            if let Some(reason) = rustel_runtime::lint::rejection(&source, false, None) {
                return Err(RuntimeError::Js(reason));
            }
            // Unlike every other command's `load_source`, a query refuses the
            // mini-notation compatibility fallback: strudel.cc has no such
            // recovery, so a score that fails JavaScript evaluation is an
            // error to report, not a source to reinterpret as bare
            // mini-notation and query anyway.
            #[cfg(feature = "device-audio")]
            session.consume_audio_confirmations();
            session.evaluate_no_fallback_cancellable(&source, &EVALUATION_CANCELLED)?;
            let failures = sample_import_failures(&source, &mut session);
            if let Some(reason) = rustel_runtime::lint::rejection_of(&source, &failures.placed)
                .or_else(|| (!failures.unplaced.is_empty()).then(|| failures.unplaced.join("; ")))
            {
                return Err(RuntimeError::Js(reason));
            }
            let begin = parse_fraction(&begin)?;
            let end = parse_fraction(&end)?;
            let report = session.query_report(begin, end)?;
            #[cfg(feature = "hydra")]
            if session
                .take_pending_hydra()
                .is_some_and(|candidate| !candidate.is_empty())
            {
                const MESSAGE: &str = "query lists pattern events only; it does not render or verify this score's Hydra visuals. Run `rustel studio` to inspect them";
                notice(
                    serde_json::json!({ "hydra": { "status": "not-rendered", "message": MESSAGE } }),
                    || format!("note: {MESSAGE}"),
                );
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(json_err)?
                );
            } else {
                print_query_report(&report);
            }
            // Print the report before returning a failure for a thrown query.
            if let Some(message) = report.query_threw {
                return Err(RuntimeError::Message(format!(
                    "the pattern threw while querying: {message}"
                )));
            }
            Ok(())
        }
        Command::Trace {
            input,
            duration,
            cps,
            device_audio,
            watch,
            json,
            score_events,
        } => {
            if watch {
                // Validate before `load_source`: `--watch -` must be rejected
                // immediately, not block forever reading stdin before the CLI
                // admits that stdin cannot be watched.
                let _ = watch_file(&input)?;
            }
            let cps = parse_cps(cps)?;
            let mut session = Session::with_config(with_sample_access(
                session_config(dispatch).with_cps(cps),
                &input.sample_access,
            )?)?;
            let live_output = LiveOutput::new(verbosity, false, json_asked());
            if watch {
                session.set_direct_diagnostic_logging(live_output.structured());
            }
            let _watcher = watch_for_interrupt(session.transport());
            if watch {
                // A leftover typo in the file must not kill `--watch`: start
                // the device, play silence, and install the next valid save.
                let (loaded, initial_error) =
                    load_watch_score(&mut session, &input, &EVALUATION_CANCELLED)?;
                return play_live(
                    &mut session,
                    &input,
                    true,
                    None,
                    &loaded,
                    initial_error,
                    None,
                    Vec::new(),
                    false,
                    score_events,
                    Vec::new(),
                    None,
                    None,
                    None,
                    None,
                    false,
                    false,
                    None,
                    live_output,
                );
            }
            // Keep exact seconds. Convert bars with the score's tempo.
            let length = duration
                .as_deref()
                .map(parse_render_length)
                .transpose()?
                .unwrap_or(RenderLength::Seconds(2.0));
            load_source(&mut session, &input, &EVALUATION_CANCELLED)?;
            let duration = match length {
                RenderLength::Seconds(seconds) => seconds,
                RenderLength::Cycles(cycles) => cycles / session.config().cps,
            };
            let duration = parse_positive_seconds("duration", duration)?;
            let report = play(&mut session, duration, device_audio)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(json_err)?
                );
            } else {
                let on = style::stdout_on();
                println!(
                    "{} {:.3} s at {} cps through {}, {} onsets",
                    style::green(on, "played"),
                    report.duration_secs,
                    report.cps,
                    style::bold(on, &report.audio_backend),
                    report.onsets.len(),
                );
            }
            // The one aside follows the same switch as every other notice, so
            // a run a script is reading never grows a sentence.
            notice(serde_json::json!({ "note": &report.note }), || {
                format!("note: {}", report.note)
            });
            // Print the report before returning a failure for a thrown query.
            if let Some(message) = report.query_threw {
                return Err(RuntimeError::Message(format!(
                    "the pattern threw while querying: {message}"
                )));
            }
            Ok(())
        }
        Command::Render {
            input,
            prebake,
            duration,
            cycles,
            until_silence,
            silence_floor,
            silence_hold,
            cps,
            output,
            format,
            sample_rate,
            // Folded into the run's one JSON switch at parse time.
            json: _,
        } => {
            let cps = parse_cps(cps)?;
            let format = format.unwrap_or_else(|| {
                output
                    .as_deref()
                    .map_or(RenderCliFormat::ScalarWav, format_for_output)
            });
            let output = match output {
                Some(output) => output,
                None => default_render_output(input.file.as_deref(), format),
            };
            let length = match (cycles, duration.as_deref()) {
                (Some(cycles), _) => {
                    RenderLength::Cycles(parse_positive_seconds("cycles", cycles)?)
                }
                (None, Some(text)) => parse_render_length(text)?,
                (None, None) => RenderLength::Cycles(DEFAULT_RENDER_CYCLES),
            };
            let mut session = Session::with_config(with_sample_access(
                session_config(dispatch)
                    .with_cps(cps)
                    .with_sample_rate(sample_rate),
                &input.sample_access,
            )?)?;
            let _watcher = watch_for_interrupt(session.transport());
            if let Some(path) = &prebake {
                let source = read_bounded_file(path, "prebake")?;
                session.evaluate_prebake_cancellable(&source, &EVALUATION_CANCELLED)?;
            }
            load_source(&mut session, &input, &EVALUATION_CANCELLED)?;
            if let Err(error) = session.enable_default_samples() {
                notice(
                    serde_json::json!({
                        "sample_library": { "status": "unavailable", "message": error.to_string() }
                    }),
                    || format!("warning: the sample library is unavailable - {error}"),
                );
            }
            let score_cps = session.config().cps;
            let (duration, whole_cycles) = length.on_a_cycle(score_cps);
            let duration = parse_positive_seconds("duration", duration)?;
            if until_silence {
                if !silence_floor.is_finite() || silence_floor > 0.0 {
                    return Err(RuntimeError::Message(format!(
                        "--silence-floor is dBFS and must be at or below 0, got {silence_floor}"
                    )));
                }
                let hold = parse_positive_seconds("silence-hold", silence_hold)?;
                let floor = 10f64.powf(silence_floor / 20.0) as f32;
                session.stop_export_when_silent(floor, std::time::Duration::from_secs_f64(hold));
                session.set_render_tail(RENDER_TAIL_CEILING_SECS);
            }
            let report = session.render(duration, &output, format.into())?;
            if json_asked() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(json_err)?
                );
            } else {
                let on = style::stdout_on();
                let seconds = report.duration_secs;
                let format_text = match format {
                    RenderCliFormat::ScalarWav => "16-bit stereo WAV",
                    RenderCliFormat::ScalarF32 => "32-bit float stereo WAV",
                    RenderCliFormat::Mp3 => "320 kbps MP3",
                    RenderCliFormat::OnsetJson => "onset JSON",
                    RenderCliFormat::Wav => "silent WAV",
                };
                println!(
                    "{} {} - {:.1} s, {} cycles{}, {} at {} Hz, {} onset{}",
                    style::green(on, "wrote"),
                    style::bold(on, &report.path),
                    seconds,
                    format_cycles(seconds * score_cps),
                    if whole_cycles { "" } else { " (as asked)" },
                    format_text,
                    report.sample_rate,
                    report.onset_count,
                    if report.onset_count == 1 { "" } else { "s" },
                );
                if until_silence {
                    println!(
                        "{}",
                        style::dim(
                            on,
                            &format!(
                                "the tail ran until the music stayed under {silence_floor} dBFS for {silence_hold} s"
                            )
                        )
                    );
                }
            }
            // Written, but not the score: see `RenderReport::failure`.
            if let Some(message) = report.failure() {
                return Err(RuntimeError::Message(message));
            }
            Ok(())
        }
        Command::Bench {
            input,
            begin,
            end,
            iterations,
            json,
        } => {
            let mut session = Session::with_config(with_sample_access(
                session_config(dispatch),
                &input.sample_access,
            )?)?;
            let _watcher = watch_for_interrupt(session.transport());
            load_source(&mut session, &input, &EVALUATION_CANCELLED)?;
            let begin = parse_fraction(&begin)?;
            let end = parse_fraction(&end)?;
            let metrics = session.bench(begin, end, iterations)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&metrics).map_err(json_err)?
                );
            } else {
                let on = style::stdout_on();
                println!(
                    "{} queries in {:.3} s - {} queries/s, {} haps/s, {} haps a query",
                    style::bold(on, &metrics.iterations.to_string()),
                    metrics.elapsed_secs,
                    style::bold(on, &format!("{:.0}", metrics.queries_per_sec)),
                    style::bold(on, &format!("{:.0}", metrics.haps_per_sec)),
                    metrics.haps_per_iteration,
                );
            }
            Ok(())
        }
        Command::Validate { input, json } => {
            let mut session = Session::with_config(with_sample_access(
                session_config(dispatch),
                &input.sample_access,
            )?)?;
            let _watcher = watch_for_interrupt(session.transport());
            load_source(&mut session, &input, &EVALUATION_CANCELLED)?;
            let source_name = match &input.file {
                Some(path) => path.display().to_string(),
                None => "<eval>".into(),
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "validate": { "status": "valid", "source": source_name }
                    })
                );
            } else {
                let on = style::stdout_on();
                println!(
                    "{} {}",
                    style::green(on, "✓ valid"),
                    style::dim(on, &source_name)
                );
            }
            Ok(())
        }
    }
}

/// The tip for a score path in the place of a command, as in
/// `rustel song.strudel`.
///
/// clap names the closest command for a misspelled one. A word with a dot or
/// a path separator, or the name of a file on disk, is a score.
fn play_tip(error: &clap::Error) -> Option<String> {
    if error.kind() != clap::error::ErrorKind::InvalidSubcommand {
        return None;
    }
    let Some(clap::error::ContextValue::String(word)) =
        error.get(clap::error::ContextKind::InvalidSubcommand)
    else {
        return None;
    };
    (word.contains(['/', '\\', '.']) || std::path::Path::new(word).exists())
        .then(|| format!("to play a score, run `rustel play {word}`"))
}

/// Print the reference entry for a name: the very body the studio's panel
/// draws, or a JSON projection of the entry for scripting.
#[cfg(feature = "studio")]
fn run_doc(name: &str, json: bool) -> Result<(), RuntimeError> {
    use rustel_studio::reference::{Reference, entry_body};
    let reference = Reference::load_all();
    let entry = reference
        .resolve(name)
        .and_then(|index| reference.entry(index))
        .cloned();
    #[cfg(feature = "hydra")]
    let entry = entry.or_else(|| {
        let name = rustel_hydra::hydra_names().find(|known| known.eq_ignore_ascii_case(name))?;
        Some(rustel_studio::reference::Entry {
            name: name.to_owned(),
            synonyms: Vec::new(),
            summary: "Hydra visual function or value.".into(),
            description: "See `rustel doc initHydra` for setup and the studio's EXAMPLES tab for Hydra snippets.".into(),
            params: Vec::new(),
            examples: Vec::new(),
            tags: vec!["hydra".into()],
            no_autocomplete: true,
            deprecated: false,
            source: String::new(),
            origin: String::new(),
            snippet: None,
            keywords: Vec::new(),
        })
    });
    let entry =
        entry.ok_or_else(|| RuntimeError::Message(format!("no reference entry for `{name}`")))?;
    if json {
        let params: Vec<serde_json::Value> = entry
            .params
            .iter()
            .map(|param| {
                serde_json::json!({
                    "name": param.name,
                    "type": param.r#type,
                    "description": param.description,
                    "choices": param.choices.iter().map(|choice| serde_json::json!({
                        "value": choice.value,
                        "description": choice.description,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        let value = serde_json::json!({
            "name": entry.name,
            "signature": entry.signature(),
            "synonyms": entry.synonyms,
            "summary": entry.summary,
            "description": entry.description,
            "params": params,
            "examples": entry.examples,
            "tags": entry.tags,
            "origin": entry.origin,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("the entry serializes")
        );
        return Ok(());
    }
    for line in entry_body(&entry, 100) {
        println!("{}", line.text);
    }
    Ok(())
}

/// The `replay` command's options, for [`run_replay`].
struct ReplayRun {
    out: Option<PathBuf>,
    speed: f64,
    from: f64,
    export: Option<PathBuf>,
    export_format: Option<ReplayExportFormat>,
    duration: Option<f64>,
    follow: bool,
    score_events: bool,
    sample_access: SampleAccessArgs,
}

/// Seconds a bounce runs past its last save when `--duration` is absent.
const REPLAY_EXPORT_TAIL_SECS: f64 = 4.0;

/// One shipped pack's row as `samples cache --list` prints it, and the JSON
/// object a script reads beside it.
struct PackRow {
    name: String,
    sounds: usize,
    files: usize,
    cached: usize,
    bytes: u64,
}

impl PackRow {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "pack": self.name,
            "sounds": self.sounds,
            "files": self.files,
            "cached": self.cached,
            "bytes": self.bytes,
        })
    }
}

/// Render `score_active` events from stdin.
/// A separate viewer can follow either a live set or a replay.
fn run_watch_code() -> Result<(), RuntimeError> {
    use std::io::BufRead as _;

    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err(RuntimeError::Message(
            "watch-code expects piped score events; try `rustel play song.strudel --watch --score-events 2>&1 | rustel watch-code`".into()
        ));
    }
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(active) = event.get("score_active") else {
            continue;
        };
        let Some(source) = active.get("source").and_then(|s| s.as_str()) else {
            continue;
        };
        let Ok(decoded) = rustel_runtime::session_log::decode_base64(source) else {
            continue;
        };
        let Ok(text) = String::from_utf8(decoded) else {
            continue;
        };
        render_active_score(
            active.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize,
            active
                .get("of")
                .and_then(|o| o.as_u64())
                .map(|of| of as usize),
            active.get("t").and_then(|t| t.as_f64()).unwrap_or(0.0),
            &text,
        );
    }
    Ok(())
}

/// Return the existing persistent recording directory.
///
/// This is not a temporary directory: the system can clear /tmp on reboot,
/// and a recorded set must survive a reboot.
fn session_dir() -> PathBuf {
    rustel_runtime::session_log::default_session_dir()
}

/// Per-generation layout delivery. A full/disconnected writer returns the
/// owned record so the 2 ms live loop can retry without reparsing the score or
/// cloning up to 64 visual option payloads.
#[cfg(feature = "device-audio")]
#[derive(Debug, Default)]
struct UiLayoutDelivery {
    observed_generation: Option<u64>,
    delivered_generation: Option<u64>,
    source_revision: Option<String>,
    audio_requested: bool,
    visual_audio_mask: u64,
    sliders: std::collections::BTreeMap<String, rustel_runtime::ui_events::UiSlider>,
    pending: Option<rustel_runtime::ui_events::UiLayoutEnvelope>,
}

#[cfg(feature = "device-audio")]
impl UiLayoutDelivery {
    fn observe(
        &mut self,
        source: &str,
        generation: u64,
    ) -> Result<bool, rustel_runtime::ui_events::UiLayoutValidationError> {
        if self.observed_generation == Some(generation) {
            return Ok(false);
        }
        self.observed_generation = Some(generation);
        self.delivered_generation = None;
        self.source_revision = None;
        self.audio_requested = false;
        self.visual_audio_mask = 0;
        self.sliders.clear();
        self.pending = None;
        let layout = rustel_runtime::ui_events::visual_layout(source, generation)?;
        self.source_revision = Some(layout.ui_layout.source_revision.clone());
        self.audio_requested = layout
            .ui_layout
            .visuals
            .iter()
            .any(|visual| matches!(visual.kind.as_str(), "scope" | "tscope" | "spectrum"));
        self.visual_audio_mask = layout
            .ui_layout
            .visuals
            .iter()
            .filter(|visual| matches!(visual.kind.as_str(), "scope" | "tscope" | "spectrum"))
            .filter_map(|visual| visual.slot)
            .fold(0, |mask, slot| mask | (1_u64 << slot));
        self.sliders.extend(
            layout
                .ui_layout
                .sliders
                .iter()
                .cloned()
                .map(|slider| (slider.id.clone(), slider)),
        );
        self.pending = Some(layout);
        Ok(true)
    }

    fn try_deliver(
        &mut self,
        mut send: impl FnMut(
            rustel_runtime::ui_events::UiLayoutEnvelope,
        ) -> Result<(), rustel_runtime::ui_events::UiLayoutEnvelope>,
    ) -> bool {
        let Some(layout) = self.pending.take() else {
            return false;
        };
        let generation = layout.ui_layout.generation;
        match send(layout) {
            Ok(()) => {
                self.delivered_generation = Some(generation);
                true
            }
            Err(layout) => {
                self.pending = Some(layout);
                false
            }
        }
    }

    fn ready_for(&self, generation: u64) -> bool {
        self.delivered_generation == Some(generation)
    }

    fn audio_requested(&self) -> bool {
        self.audio_requested
    }

    fn set_slider_value(&mut self, id: &str, value: f64) {
        if let Some(slider) = self.sliders.get_mut(id) {
            slider.value = value;
        }
        if let Some(layout) = self.pending.as_mut()
            && let Some(slider) = layout
                .ui_layout
                .sliders
                .iter_mut()
                .find(|slider| slider.id == id)
        {
            slider.value = value;
        }
    }

    fn sync_slider_values(&mut self, session: &Session) {
        let ids = self.sliders.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let Some(slider) = self.sliders.get(&id) else {
                continue;
            };
            let Ok(Some(value)) = session.slider_value(&id) else {
                continue;
            };
            if value >= slider.min && value <= slider.max {
                self.set_slider_value(&id, value);
            }
        }
    }
}

#[cfg(feature = "device-audio")]
const UI_CONTROL_PROTOCOL_VERSION: u16 = 1;
#[cfg(feature = "device-audio")]
const MAX_UI_CONTROL_LINE_BYTES: usize = 16 * 1024;
#[cfg(feature = "device-audio")]
const MAX_PENDING_UI_SLIDERS: usize = rustel_runtime::ui_events::MAX_UI_LAYOUT_SLIDERS;

#[cfg(feature = "device-audio")]
#[derive(Clone, Debug, serde::Deserialize)]
struct UiSliderControlEnvelope {
    ui_control: UiSliderControl,
}

#[cfg(feature = "device-audio")]
#[derive(Clone, Debug, serde::Deserialize)]
struct UiSliderControl {
    version: u16,
    kind: String,
    generation: u64,
    source_revision: String,
    id: String,
    value: f64,
}

#[cfg(feature = "device-audio")]
impl UiSliderControl {
    fn wire_valid(&self) -> bool {
        self.version == UI_CONTROL_PROTOCOL_VERSION
            && self.kind == "slider"
            && self.source_revision.len() == 64
            && self
                .source_revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            && !self.id.is_empty()
            && self.id.len() <= rustel_runtime::ui_events::MAX_UI_LAYOUT_ID_BYTES
            && self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-'))
            && self.value.is_finite()
    }
}

#[cfg(feature = "device-audio")]
#[derive(Clone, Debug, Default)]
struct UiControlInbox {
    pending: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, UiSliderControl>>>,
}

#[cfg(feature = "device-audio")]
impl UiControlInbox {
    fn stdin() -> io::Result<Self> {
        let inbox = Self::default();
        let pending = std::sync::Arc::clone(&inbox.pending);
        std::thread::Builder::new()
            .name("ui-control-reader".to_owned())
            .spawn(move || read_ui_controls(std::io::stdin(), &pending))?;
        Ok(inbox)
    }

    fn drain(&self) -> Vec<UiSliderControl> {
        // A poisoned mutex must not disable controls for the rest of the set:
        // the map holds only plain data, so recover it like the senders do.
        let mut pending = match self.pending.try_lock() {
            Ok(pending) => pending,
            Err(std::sync::TryLockError::WouldBlock) => return Vec::new(),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        std::mem::take(&mut *pending).into_values().collect()
    }
}

#[cfg(feature = "device-audio")]
#[derive(Clone, Debug, Eq, PartialEq)]
enum UiSliderApplyStatus {
    Applied,
    Stale,
    Unknown,
    OutOfRange,
    /// The id is unknown to the runtime (never registered or already gone).
    RuntimeRejected,
    /// The runtime cell exists but the mutation failed inside QuickJS.
    RuntimeFailed(String),
}

/// A query-time control has changed the Session graph, but that generation has
/// not yet proved schedulable and reached the audio consumer.  During this
/// short transactional window the only layout a client may legitimately have
/// is still the audible one.  Retaining both ends lets a corrective value from
/// that exact layout supersede a failed control generation without relaxing
/// source-revision validation for ordinary stale input.
#[cfg(feature = "device-audio")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingUiControlCutover {
    audible_generation: u64,
    session_generation: u64,
}

#[cfg(feature = "device-audio")]
impl PendingUiControlCutover {
    fn corrective_generation(
        self,
        session_generation: u64,
        audible_generation: u64,
    ) -> Option<u64> {
        (self.session_generation == session_generation
            && self.audible_generation == audible_generation
            && session_generation != audible_generation)
            .then_some(self.audible_generation)
    }
}

/// The external intents one live tick dispatches.
#[cfg(all(
    feature = "device-audio",
    any(feature = "midi", feature = "osc", feature = "serial")
))]
struct TickIntents {
    #[cfg(feature = "midi")]
    midi: Vec<rustel_runtime::midi_bridge::MidiOnset>,
    #[cfg(feature = "osc")]
    osc: Vec<(f64, rustel_runtime::osc_bridge::OscOnset)>,
    #[cfg(feature = "serial")]
    serial: Vec<(f64, rustel_runtime::serial_bridge::SerialOnset)>,
}

fn load_musician_sources(
    session: &mut Session,
    input: &SourceInput,
    prebake: Option<&std::path::Path>,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Result<LoadedLiveSources, RuntimeError> {
    let prebake = if let Some(path) = prebake {
        let source = read_bounded_file(path, "prebake")?;
        #[cfg(feature = "device-audio")]
        session.consume_audio_confirmations();
        session.evaluate_prebake_cancellable(&source, cancellation)?;
        Some((path.to_path_buf(), source))
    } else {
        None
    };
    let score = read_source(input)?;
    #[cfg(feature = "device-audio")]
    session.consume_audio_confirmations();
    session.evaluate_playable_cancellable(&score, cancellation)?;
    Ok(LoadedLiveSources { score, prebake })
}

fn load_watch_score(
    session: &mut Session,
    input: &SourceInput,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Result<(LoadedLiveSources, Option<RuntimeError>), RuntimeError> {
    let score = read_source(input)?;
    let error = evaluate_source(session, &score, cancellation).err();
    Ok((
        LoadedLiveSources {
            score,
            prebake: None,
        },
        error,
    ))
}

fn load_watch_musician_sources(
    session: &mut Session,
    input: &SourceInput,
    prebake: Option<&std::path::Path>,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Result<(LoadedLiveSources, Option<RuntimeError>), RuntimeError> {
    // Under `--watch`, no fault in the starting files ends the process. An
    // oversized or unreadable file, or a prebake that throws, is handled like
    // a save that does not parse: the artist is about to edit the file. The
    // clock keeps running, the set plays silence, and the next save installs.
    let mut startup_error = None;
    let prebake = match prebake {
        Some(path) => match read_bounded_file(path, "prebake") {
            Ok(source) => {
                #[cfg(feature = "device-audio")]
                session.consume_audio_confirmations();
                if let Err(error) = session.evaluate_prebake_cancellable(&source, cancellation) {
                    startup_error = Some(error);
                }
                Some((path.to_path_buf(), source))
            }
            Err(error) => {
                startup_error = Some(error);
                // Remember the path so a later save to it is still watched.
                Some((path.to_path_buf(), String::new()))
            }
        },
        None => None,
    };
    let score = match read_source(input) {
        Ok(score) => score,
        Err(error) => {
            startup_error = startup_error.or(Some(error));
            // No text stands in for the unreadable file, so the first save
            // that differs from it installs.
            String::new()
        }
    };
    let error = match evaluate_source(session, &score, cancellation).err() {
        // The evaluation error is the more specific one when both happened.
        Some(error) => Some(error),
        None => startup_error,
    };
    Ok((LoadedLiveSources { score, prebake }, error))
}

fn read_source(input: &SourceInput) -> Result<String, RuntimeError> {
    match (&input.file, &input.eval) {
        // `-` reads the source from stdin. The blocking read also lets a signal
        // received before evaluation remain observable when the pipe closes.
        (Some(path), None) if path.as_os_str() == "-" => {
            read_bounded(std::io::stdin().lock(), "stdin source")
        }
        (Some(path), None) => read_bounded_file(path, "source"),
        (None, Some(expr)) => Ok(expr.clone()),
        (Some(_), Some(_)) => Err(RuntimeError::Message(
            "provide either a file or -e/--eval, not both".into(),
        )),
        (None, None) => Err(RuntimeError::Message(
            "provide a source file or -e/--eval expression".into(),
        )),
    }
}

fn evaluate_source(
    session: &mut Session,
    source: &str,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Result<(), RuntimeError> {
    #[cfg(feature = "device-audio")]
    session.consume_audio_confirmations();
    // A full score is JavaScript here; quoted mini-notation strings are
    // compiled natively during evaluation, and a JS failure on a bare
    // pattern falls back to the Rust mini parser.
    session.evaluate_cancellable(source, cancellation)
}

fn load_source(
    session: &mut Session,
    input: &SourceInput,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Result<String, RuntimeError> {
    let source = read_source(input)?;
    evaluate_source(session, &source, cancellation)?;
    Ok(source)
}

/// Setup and score files are untrusted allocation inputs. The live watcher
/// already enforces the same four-MiB ceiling; initial loading must not leave a
/// larger hole merely because it happens before the watcher starts.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

fn read_bounded_file(path: &std::path::Path, what: &str) -> Result<String, RuntimeError> {
    // The error names the file: "No such file or directory" alone sends a
    // person hunting for which of the arguments was the path.
    let file = std::fs::File::open(path).map_err(|error| {
        RuntimeError::Io(std::io::Error::new(
            error.kind(),
            format!("cannot open the {what} {}: {error}", path.display()),
        ))
    })?;
    if file.metadata()?.len() > MAX_SOURCE_BYTES {
        return Err(RuntimeError::ResourceLimit(format!(
            "{what} {} exceeds the {MAX_SOURCE_BYTES} byte limit",
            path.display()
        )));
    }
    read_bounded(file, &format!("{what} {}", path.display()))
}

fn read_bounded(reader: impl std::io::Read, what: &str) -> Result<String, RuntimeError> {
    let mut bytes = Vec::new();
    reader.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(RuntimeError::ResourceLimit(format!(
            "{what} exceeds the {MAX_SOURCE_BYTES} byte limit"
        )));
    }
    String::from_utf8(bytes).map_err(|error| {
        RuntimeError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })
}

/// The longest `play`/`render` window this build will schedule.
///
/// `play` accumulates one entry per onset for the whole window before it
/// returns, so the duration is an unbounded allocation input in exactly the way
/// a query span is - and `Fraction::from_f64`/`span_cycles` already carry the
/// same class of guard for that reason.
///
/// A finite value can still be unsafe: `--duration 1e300` would step a ~0.05s
/// virtual clock roughly 10^301 times while appending onsets. The explicit cap
/// keeps both runtime and allocation bounded.
///
/// Twenty-four hours is longer than any practical offline render. Indefinite
/// `--watch` playback is a different code path.
const MAX_DURATION_SECS: f64 = 24.0 * 60.0 * 60.0;

/// A wall-clock duration or a cycles-per-second rate, from the command line.
///
/// Every one of these drives a loop bound or a divisor, so a non-finite or
/// negative value is not a cosmetic complaint: `--duration inf` would keep
/// `play` spinning forever against its virtual clock.
/// The length of a bounce as the command line says it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RenderLength {
    Seconds(f64),
    Cycles(f64),
}

/// The bounce length when none is given: eight cycles, which is sixteen
/// seconds at the default tempo.
const DEFAULT_RENDER_CYCLES: f64 = 8.0;
/// How far past its length a bounce may run for its tail before giving up
/// on silence: a looping score never goes quiet.
const RENDER_TAIL_CEILING_SECS: f64 = 60.0;

impl RenderLength {
    /// The length in seconds, ended on a whole cycle: a count of cycles as
    /// it is, a time of a cycle or more rounded to the nearest cycle
    /// boundary, so a bounce never stops mid-bar. Less than a cycle is
    /// deliberate - a hit, a test - and stays as asked. The flag says
    /// whether the length is whole cycles.
    fn on_a_cycle(self, cps: f64) -> (f64, bool) {
        match self {
            Self::Cycles(cycles) => (cycles / cps, true),
            Self::Seconds(seconds) => {
                if !(cps.is_finite() && cps > 0.0) {
                    return (seconds, false);
                }
                let cycle = 1.0 / cps;
                if seconds < cycle {
                    return (seconds, false);
                }
                let cycles = (seconds / cycle).round();
                (((cycles * cycle) * 1000.0).round() / 1000.0, true)
            }
        }
    }
}

/// `60`, `30s`, `2m`, `1:30`, `1h`, or `16b` for bars (cycles): how long a
/// bounce should be, as a person writes it.
fn parse_render_length(text: &str) -> Result<RenderLength, RuntimeError> {
    let text = text.trim();
    let bad = || {
        RuntimeError::Message(format!(
            "--duration {text:?}: write seconds (60, 30s), minutes (2m, 1:30), hours (1h) or bars (16b)"
        ))
    };
    if let Some((minutes, seconds)) = text.split_once(':') {
        let minutes: f64 = minutes.trim().parse().map_err(|_| bad())?;
        let seconds: f64 = seconds.trim().parse().map_err(|_| bad())?;
        if !(minutes.is_finite() && seconds.is_finite()) || minutes < 0.0 || seconds < 0.0 {
            return Err(bad());
        }
        return Ok(RenderLength::Seconds(minutes * 60.0 + seconds));
    }
    let split = text
        .trim_end_matches(|character: char| character.is_ascii_alphabetic())
        .len();
    let (number, unit) = text.split_at(split);
    let number: f64 = number.trim().parse().map_err(|_| bad())?;
    if !number.is_finite() || number < 0.0 {
        return Err(bad());
    }
    match unit.trim().to_ascii_lowercase().as_str() {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => Ok(RenderLength::Seconds(number)),
        "m" | "min" | "mins" | "minute" | "minutes" => Ok(RenderLength::Seconds(number * 60.0)),
        "h" | "hr" | "hour" | "hours" => Ok(RenderLength::Seconds(number * 3600.0)),
        "b" | "bar" | "bars" | "c" | "cycle" | "cycles" => Ok(RenderLength::Cycles(number)),
        _ => Err(bad()),
    }
}

/// The format an output name asks for: `.mp3` is mp3, `.json` the onset
/// dump, anything else the normal bounce.
fn format_for_output(output: &std::path::Path) -> RenderCliFormat {
    match output
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp3") => RenderCliFormat::Mp3,
        Some("json") => RenderCliFormat::OnsetJson,
        _ => RenderCliFormat::ScalarWav,
    }
}

/// Where a bounce goes when nobody said: beside the score, its name with
/// the format's extension; `render.wav` in the working directory for an
/// inline expression.
fn default_render_output(score: Option<&std::path::Path>, format: RenderCliFormat) -> PathBuf {
    let extension = match format {
        RenderCliFormat::Mp3 => "mp3",
        RenderCliFormat::OnsetJson => "json",
        _ => "wav",
    };
    match score {
        Some(score) => score.with_extension(extension),
        None => PathBuf::from(format!("render.{extension}")),
    }
}

/// A count of cycles as a line of text says it: whole when it is.
fn format_cycles(cycles: f64) -> String {
    if (cycles - cycles.round()).abs() < 0.005 {
        format!("{}", cycles.round() as i64)
    } else {
        format!("{cycles:.2}")
    }
}

fn parse_positive_seconds(what: &str, value: f64) -> Result<f64, RuntimeError> {
    if !value.is_finite() {
        return Err(RuntimeError::Message(format!(
            "{what} must be a finite number, got {value}"
        )));
    }
    if value < 0.0 {
        return Err(RuntimeError::Message(format!(
            "{what} must not be negative, got {value}"
        )));
    }
    if value > MAX_DURATION_SECS {
        // A LIMIT, not a malformed argument: the caller may sensibly retry with
        // a shorter window, so it carries the typed variant and its own exit
        // code rather than being lumped in with a typo.
        return Err(RuntimeError::ResourceLimit(format!(
            "{what} must be at most {MAX_DURATION_SECS} seconds ({:.0} hours), got {value}",
            MAX_DURATION_SECS / 3600.0
        )));
    }
    Ok(value)
}

/// `cps` additionally cannot be zero: it divides.
fn parse_cps(value: f64) -> Result<f64, RuntimeError> {
    let value = parse_positive_seconds("cps", value)?;
    if value == 0.0 {
        return Err(RuntimeError::Message(
            "cps must be greater than zero: a cycle rate of zero never advances".into(),
        ));
    }
    Ok(value)
}

/// Bound typed `--begin`/`--end` times to 2^63 cycles, leaving room for pattern
/// scaling instead of accepting cycle counts across the full i128 range.
const MAX_CYCLE_TIME: i128 = 1 << 63;

fn cycle_time_in_range(text: &str, value: Fraction) -> Result<Fraction, RuntimeError> {
    if value > Fraction::int(MAX_CYCLE_TIME) || value < Fraction::int(-MAX_CYCLE_TIME) {
        return Err(RuntimeError::Message(format!(
            "cycle time {text:?} is out of range: a cycle time must lie within 2^63 cycles of zero"
        )));
    }
    Ok(value)
}

fn parse_fraction(text: &str) -> Result<Fraction, RuntimeError> {
    let text = text.trim();
    if let Some((n, d)) = text.split_once('/') {
        let n: i128 = n
            .trim()
            .parse()
            .map_err(|e| RuntimeError::Message(format!("bad numerator in {text:?}: {e}")))?;
        let d: i128 = d
            .trim()
            .parse()
            .map_err(|e| RuntimeError::Message(format!("bad denominator in {text:?}: {e}")))?;
        // Give zero denominators a specific argument error before construction.
        if d == 0 {
            return Err(RuntimeError::Message(format!(
                "zero denominator in {text:?}: a cycle time cannot be divided by zero"
            )));
        }
        // Sign normalization can require an unrepresentable positive 2^127.
        // Checked construction reports an argument error instead of panicking.
        let value = Fraction::checked_new(n, d).ok_or_else(|| {
            RuntimeError::Message(format!(
                "cycle time {text:?} is out of range: it has no exact 128-bit fraction"
            ))
        })?;
        return cycle_time_in_range(text, value);
    }
    let v: f64 = text
        .parse()
        .map_err(|e| RuntimeError::Message(format!("bad cycle time {text:?}: {e}")))?;
    // A non-finite or out-of-range value would saturate the `as i128` cast and
    // then be used as a cycle count, potentially requesting an enormous span.
    if !v.is_finite() {
        return Err(RuntimeError::Message(format!(
            "cycle time {text:?} is not a finite number"
        )));
    }
    // Inside the bound, the micro-rational below fits i128 with room to
    // spare, so the scaled cast cannot saturate.
    if v.abs() > MAX_CYCLE_TIME as f64 {
        return Err(RuntimeError::Message(format!(
            "cycle time {text:?} is out of range: a cycle time must lie within 2^63 cycles of zero"
        )));
    }
    let scaled = v * 1_000_000.0;
    // Match the QuickJS binding's micro-rational encoding for decimal inputs.
    Ok(Fraction::new(scaled.round() as i128, 1_000_000))
}

#[cfg(test)]
mod parse_fraction_tests {
    use super::{Fraction, parse_fraction};

    /// A denominator of i128::MIN (magnitude 2^127) has no exact signed-i128
    /// normal form. A mistyped argument must be diagnosed and must not panic.
    #[test]
    fn unrepresentable_ratios_are_refused_not_panicked() {
        for text in [
            // denominator 2^127 with an odd numerator: gcd cannot shrink it
            "1/-170141183460469231731687303715884105728",
            // numerator 2^127 that must come out POSITIVE: no signed i128 holds it
            "-170141183460469231731687303715884105728/-3",
        ] {
            let err = parse_fraction(text)
                .err()
                .unwrap_or_else(|| panic!("{text:?} must be refused, not constructed"));
            assert!(
                format!("{err:?}").contains("out of range"),
                "the refusal did not name the range: {err:?}"
            );
        }
    }

    /// A value that fits i128 but not the engine's headroom is refused too:
    /// `i128::MIN/1` parsed, then panicked the query at `"c d"`'s `fast(2)`.
    #[test]
    fn cycle_times_past_two_to_the_63_are_refused() {
        for text in [
            "-170141183460469231731687303715884105728/1",
            "170141183460469231731687303715884105726/1",
            "9223372036854775809/1",
            "-9.3e18",
            "1e19",
            "-1e19",
        ] {
            let err = parse_fraction(text)
                .err()
                .unwrap_or_else(|| panic!("{text:?} must be refused, not accepted"));
            assert!(
                format!("{err:?}").contains("out of range"),
                "the refusal did not name the range: {err:?}"
            );
        }
        // The bound itself is a cycle time like any other.
        assert_eq!(
            parse_fraction("9223372036854775808/1").unwrap(),
            Fraction::int(1 << 63)
        );
        assert_eq!(
            parse_fraction("-9223372036854775808/1").unwrap(),
            Fraction::int(-(1 << 63))
        );
    }

    /// The legal shapes around the edge keep their exact values and messages.
    #[test]
    fn representable_rationals_parse_unchanged() {
        assert_eq!(parse_fraction("1/-6").unwrap(), Fraction::new(1, -6));
        assert_eq!(parse_fraction("1/3").unwrap(), Fraction::new(1, 3));
        assert_eq!(parse_fraction("0.25").unwrap(), Fraction::new(1, 4));
        assert_eq!(
            parse_fraction("1e12").unwrap(),
            Fraction::int(1_000_000_000_000)
        );
        assert_eq!(
            parse_fraction("0/-170141183460469231731687303715884105728").unwrap(),
            Fraction::ZERO
        );
        // The zero-denominator refusal keeps its own message.
        let zero = parse_fraction("1/0").unwrap_err();
        assert!(
            format!("{zero:?}").contains("zero denominator"),
            "the zero-denominator message moved: {zero:?}"
        );
    }
}

/// One event per line: when it sounds, and what it says.
///
/// The JSON is what a machine reads and stays exactly as it was; this is the
/// same report for the person who ran the command, who wants to see whether
/// the notes land where they meant them to and does not want to count braces
/// to find out.
fn print_query_report(report: &rustel_runtime::QueryReport) {
    let on = style::stdout_on();
    println!("{}", style::dim(on, &style::safe_source(&report.source)));
    // The spans are rationals of wildly different widths - `0/1` beside
    // `11/16` - so the arrow only lines up if the column is measured first.
    let width = report
        .haps
        .iter()
        .map(|hap| span_text(hap).chars().count())
        .max()
        .unwrap_or(0);
    for hap in &report.haps {
        // Anything that does not begin where its whole begins: a hap the
        // query window or a combinator cut, and a continuous signal, which has
        // no whole to begin at. Neither is a new note, so neither takes an
        // onset's column.
        let onset = if hap.has_onset { " " } else { "·" };
        // The span is padded before it is painted: an escape has no width on
        // screen but plenty in a format string, and `{:width$}` counts bytes.
        let span = format!("{:width$}", span_text(hap));
        println!(
            "{onset} {span}  {value}",
            onset = style::dim(on, onset),
            span = style::cyan(on, &span),
            value = value_text(on, &hap.value),
        );
    }
    let count = report.haps.len();
    let plural = if count == 1 { "event" } else { "events" };
    println!(
        "{}",
        style::dim(
            on,
            &format!("{count} {plural} in {} → {}", report.begin, report.end)
        )
    );
}

/// Sample import failures for one-shot `check` and `query` commands.
struct SampleImportFailures {
    /// Failures with a source location.
    placed: Vec<rustel_runtime::lint::Diagnostic>,
    /// Failures without a source location.
    unplaced: Vec<String>,
}

/// Wait up to five seconds for sample maps, then collect import failures.
fn sample_import_failures(source: &str, session: &mut Session) -> SampleImportFailures {
    use rustel_runtime::samples::SourceState;
    let imports = rustel_runtime::sounds::samples_imports(source);
    let is_local = |import: &rustel_runtime::sounds::SamplesImport| {
        import
            .spec
            .as_deref()
            .is_some_and(|spec| spec.starts_with("local:"))
    };
    let loading = |library: &rustel_runtime::samples::SampleLibrary,
                   import: &rustel_runtime::sounds::SamplesImport| {
        import.spec.as_deref().is_some_and(|spec| {
            matches!(
                library.samples_source_state(spec),
                Some(SourceState::Loading)
            )
        })
    };
    if session
        .sample_library()
        .is_some_and(|library| imports.iter().any(|import| loading(library, import)))
    {
        session.wait_for_sample_loads(std::time::Duration::from_secs(5));
    }
    let mut failures = SampleImportFailures {
        placed: Vec::new(),
        unplaced: Vec::new(),
    };
    for diagnostic in session.take_diagnostics() {
        if diagnostic.kind != "samples-failed" {
            notice(
                serde_json::json!({"diagnostic": {"kind": diagnostic.kind, "message": diagnostic.message}}),
                || diagnostic.message,
            );
            continue;
        }
        // Assign local refusals to the first local import. Assign other
        // refusals only when there is one remote import.
        let local = diagnostic
            .message
            .contains("requires a host-selected local sample root");
        let mut candidates = imports.iter().filter(|import| is_local(import) == local);
        let concerned = match (candidates.next(), candidates.next()) {
            (Some(first), _) if local => Some(first),
            (Some(only), None) => Some(only),
            _ => None,
        };
        let message = if local {
            format!(
                "{} (pass --allow-local-samples DIR to grant access)",
                diagnostic.message
            )
        } else {
            diagnostic.message
        };
        match concerned {
            Some(import) => failures.placed.push(rustel_runtime::lint::Diagnostic {
                level: rustel_runtime::lint::Level::Value,
                message,
                from: import.from,
                to: import.to,
            }),
            None => failures.unplaced.push(message),
        }
    }
    let Some(library) = session.sample_library() else {
        return failures;
    };
    let unread = rustel_runtime::lint::failed_samples_imports(&imports, library).chain(
        imports
            .iter()
            .filter(|import| is_local(import) && loading(library, import))
            .map(|import| rustel_runtime::lint::Diagnostic {
                level: rustel_runtime::lint::Level::Value,
                message: "samples: local sample import is still loading after 5 seconds".to_owned(),
                from: import.from,
                to: import.to,
            }),
    );
    for finding in unread {
        if !failures
            .placed
            .iter()
            .any(|placed| placed.from == finding.from && placed.to == finding.to)
        {
            failures.placed.push(finding);
        }
    }
    failures
}

/// The span a reader cares about: where the note begins and ends, which is its
/// `whole` where it has one and the queried fragment where it does not.
fn span_text(hap: &rustel_runtime::HapJson) -> String {
    let span = hap.whole.as_ref().unwrap_or(&hap.part);
    format!("{} → {}", span.begin, span.end)
}

/// `s:bd note:c gain:0.8` - the controls in the order the JSON lists them, so
/// the two views of one hap read the same way round.
///
/// The names are dimmed and the values are not, because a line is read for its
/// values: `bd`, `c`, `0.8` are what changed between one event and the next.
fn value_text(on: bool, value: &rustel_runtime::ValueJson) -> String {
    match value {
        rustel_runtime::ValueJson::Null => style::dim(on, "~"),
        rustel_runtime::ValueJson::Bool(flag) => flag.to_string(),
        rustel_runtime::ValueJson::Number(number) => number.to_string(),
        rustel_runtime::ValueJson::String(text) => terminal_text::visible(text),
        rustel_runtime::ValueJson::Raw(serde_json::Value::Object(fields)) => fields
            .iter()
            .map(|(name, field)| {
                let name = style::dim(on, &format!("{}:", terminal_text::visible(name)));
                match field {
                    serde_json::Value::String(text) => {
                        format!("{name}{}", terminal_text::visible(text))
                    }
                    other => format!("{name}{}", terminal_text::visible(&other.to_string())),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        rustel_runtime::ValueJson::Raw(other) => terminal_text::visible(&other.to_string()),
    }
}

static NO_COLOR_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NO_INPUT_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static QUIET_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn color_disabled() -> bool {
    NO_COLOR_MODE.load(std::sync::atomic::Ordering::Relaxed)
        || std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}

fn quiet_asked() -> bool {
    QUIET_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether this run asked for JSON - the command's own `--json`, a command
/// whose only output is JSON, or the verbosity that means the JSON event
/// stream - decided once from the parsed line. Every notice, progress line
/// and the error envelope follow it, so a run is text or JSON and never both.
static JSON_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn json_asked() -> bool {
    JSON_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

impl Cli {
    fn checks_for_updates(&self, terminal: bool, ci: bool) -> bool {
        if !terminal
            || ci
            || self.no_update_check
            || self.no_input
            || self.quiet
            || self.verbose >= LiveOutput::JSON_VERBOSITY
            || self.wants_json()
        {
            return false;
        }
        match &self.command {
            Command::Play(musician) => {
                !musician.ui_events && !musician.announce_score && !musician.follow
            }
            // `samples` draws a progress bar or a prompt, and `serve-samples`
            // writes only JSON. A late notice breaks each of them.
            Command::Config { .. }
            | Command::Completions { .. }
            | Command::WatchCode
            | Command::Samples { .. }
            | Command::ServeSamples { .. } => false,
            Command::Replay {
                follow,
                score_events,
                ..
            } => !follow && !score_events,
            Command::Trace { score_events, .. } => !score_events,
            #[cfg(feature = "studio")]
            Command::Studio {
                list_themes,
                probe_terminal,
                performance_events,
                ..
            } => !list_themes && !probe_terminal && !performance_events,
            _ => true,
        }
    }

    /// Whether this run speaks JSON: the chosen command's own `--json`, and
    /// the live playback/replay verbosity that means the JSON event stream.
    fn wants_json(&self) -> bool {
        self.command.wants_json()
            || (self.verbose >= LiveOutput::JSON_VERBOSITY
                && matches!(self.command, Command::Play(_) | Command::Replay { .. }))
    }

    /// The flags of a parsed `play` line.
    #[cfg(test)]
    fn play(&self) -> &MusicianArgs {
        match &self.command {
            Command::Play(musician) => musician,
            other => panic!("not a play command: {other:?}"),
        }
    }
}

impl Command {
    /// The command's own `--json` where it has one, and true outright where
    /// the command has no other form.
    fn wants_json(&self) -> bool {
        match self {
            Self::Play(musician) => musician.json,
            Self::MidiMonitor { json, .. }
            | Self::Devices { json, .. }
            | Self::Doctor { json, .. }
            | Self::MidiList { json, .. }
            | Self::Check { json, .. }
            | Self::Query { json, .. }
            | Self::Trace { json, .. }
            | Self::Bench { json, .. }
            | Self::Validate { json, .. }
            | Self::Render { json, .. }
            | Self::Replay { json, .. } => *json,
            Self::ClearScoreCache { .. } => true,
            Self::Samples { command } => command.wants_json(),
            #[cfg(feature = "studio")]
            Self::Doc { json, .. } => *json,
            _ => false,
        }
    }
}

/// A notice on stderr: the JSON object when the run asked for JSON, the
/// sentence otherwise - one or the other.
fn notice(json: serde_json::Value, text: impl FnOnce() -> String) {
    if quiet_asked() {
        return;
    }
    if json_asked() {
        eprintln!("{json}");
    } else {
        eprintln!("{}", style::yellow(style::stderr_on(), &text()));
    }
}

fn json_err(err: serde_json::Error) -> RuntimeError {
    RuntimeError::Message(err.to_string())
}

#[cfg(test)]
mod dsp_selection_tests {
    use super::*;
    use clap::CommandFactory;

    // Observe configuration actually built by the command, not just its
    // arguments: the selected kernels intentionally produce identical PCM.
    std::thread_local! {
        pub(super) static BUILT_DISPATCH: std::cell::Cell<Option<rustel_audio::DspDispatch>> = const {
            std::cell::Cell::new(None)
        };
    }

    #[test]
    fn acceleration_parses_once_for_root_and_subcommand_routes() {
        for args in [
            vec![
                product::COMMAND_NAME,
                "--acceleration",
                "portable",
                "play",
                "song.strudel",
            ],
            vec![
                product::COMMAND_NAME,
                "play",
                "song.strudel",
                "--acceleration",
                "portable",
            ],
            vec![
                product::COMMAND_NAME,
                "doctor",
                "--acceleration",
                "portable",
            ],
            vec![
                product::COMMAND_NAME,
                "replay",
                "set.jsonl",
                "--acceleration",
                "portable",
            ],
        ] {
            let cli = Cli::try_parse_from(args).expect("portable command");
            let config = session_config(cli.acceleration.dispatch());
            assert!(config.dsp_dispatch.is_forced_portable());
        }
        let cli = Cli::try_parse_from([product::COMMAND_NAME, "doctor"]).expect("default");
        let config = session_config(cli.acceleration.dispatch());
        let defaults = SessionConfig::default();
        assert!(!config.dsp_dispatch.is_forced_portable());
        assert_eq!(
            (config.cps, config.sample_rate, config.channels),
            (defaults.cps, defaults.sample_rate, defaults.channels)
        );
        for unsupported in ["avx2", "neon", "unknown"] {
            assert!(
                Cli::try_parse_from([
                    product::COMMAND_NAME,
                    "doctor",
                    "--acceleration",
                    unsupported,
                ])
                .is_err()
            );
        }
        let help = Cli::command()
            .render_long_help()
            .to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(help.contains("convolution, supersaw and wavetable"));
        assert!(help.contains("does not disable RustFFT or compiler-generated SIMD"));
    }

    #[test]
    fn command_and_replay_exports_retain_prepared_configuration() {
        use rustel_runtime::session_log::{SaveStatus, SessionMode, SessionRecorder};

        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("export directory");
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../runtime/tests/e2e/scores/corpus/regressions/supersaw-plain.strudel");
        let tape = directory.path().join("set.jsonl");
        let mut recorder =
            SessionRecorder::create(tape.clone(), SessionMode::Normal, None).expect("test tape");
        recorder.record_save(
            0.0,
            SaveStatus::Installed,
            include_str!(
                "../../runtime/tests/e2e/scores/corpus/regressions/supersaw-plain.strudel"
            ),
            None,
        );
        drop(recorder);

        for route in ["render", "replay"] {
            let mut expected = None;
            for preference in ["auto", "portable"] {
                let output = directory.path().join(format!("{route}-{preference}.wav"));
                assert!(!output.exists(), "export destination must be fresh");
                let mut args = vec![product::COMMAND_NAME.to_owned()];
                match route {
                    "render" => args.extend([
                        "render".into(),
                        fixture.display().to_string(),
                        "--format".into(),
                        "scalar-wav".into(),
                        "--output".into(),
                        output.display().to_string(),
                    ]),
                    "replay" => args.extend([
                        "replay".into(),
                        tape.display().to_string(),
                        "--export".into(),
                        output.display().to_string(),
                        "--out".into(),
                        directory
                            .path()
                            .join(format!("replay-{preference}.strudel"))
                            .display()
                            .to_string(),
                    ]),
                    _ => unreachable!(),
                }
                args.extend([
                    "--duration".into(),
                    "0.125".into(),
                    "--acceleration".into(),
                    preference.into(),
                ]);
                let cli = Cli::try_parse_from(args).expect("export command");
                let dispatch = cli.acceleration.dispatch();
                BUILT_DISPATCH.set(None);
                run_command(cli.command, cli.verbose, dispatch, false).expect("selected export");
                let actual = BUILT_DISPATCH
                    .take()
                    .expect("command constructed its SessionConfig");
                assert_eq!(
                    rustel_runtime::capability_registry_for_dispatch(actual),
                    rustel_runtime::capability_registry_for_dispatch(dispatch)
                );
                let bytes = std::fs::read(output).expect("exported WAV");
                let decoded = rustel_audio::decode_wav(&bytes).expect("valid WAV");
                assert_eq!(decoded.pcm().len(), 12_000);
                assert!(decoded.pcm().iter().all(|sample| sample.is_finite()));
                assert!(decoded.pcm().iter().any(|sample| sample.abs() > 1e-6));
                if let Some(expected) = &expected {
                    assert_eq!(&bytes, expected);
                } else {
                    expected = Some(bytes);
                }
            }
        }
    }

    #[test]
    fn unavailable_doctor_keeps_the_configured_selection_without_audio_facts() {
        let report = doctor_unavailable_report(rustel_audio::DspDispatch::portable(), "not opened");
        assert_eq!(report.runtime.acceleration_preference, "portable");
        assert!(report.audio.is_none());
        assert_eq!(
            report.audio_unavailable_reason.as_deref(),
            Some("not opened")
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn live_options_and_doctor_follow_the_actual_device_through_recycle() {
        for dispatch in [
            rustel_audio::DspDispatch::automatic(),
            rustel_audio::DspDispatch::portable(),
        ] {
            let session = Session::with_config(session_config(dispatch)).expect("session");
            let mut device = rustel_audio::LiveScalarDevice::start_output_with_options(
                Some(rustel_audio::SILENT_OUTPUT_NAME),
                session.generation(),
                live_output_options(&session, None),
            )
            .expect("silent output");
            for recycled in [false, true] {
                if recycled {
                    device.recycle_output().expect("recycle");
                }
                assert_eq!(
                    rustel_runtime::capability_registry_for_dispatch(device.dispatch()),
                    rustel_runtime::capability_registry_for_dispatch(dispatch)
                );
                let report = doctor_device_report(&device, false);
                assert_eq!(
                    report.runtime.acceleration_preference,
                    if dispatch.is_forced_portable() {
                        "portable"
                    } else {
                        "auto"
                    }
                );
                assert_eq!(report.audio.expect("opened stream").host.kind, "silent");
                assert!(report.audio_unavailable_reason.is_none());
            }
        }
    }
}

#[cfg(all(
    test,
    feature = "device-audio",
    feature = "midi",
    feature = "osc",
    feature = "serial"
))]
mod tick_intent_tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    /// A successful tick forwards MIDI, OSC and serial intents together.
    /// A failed tick clears all three so no stale intent reaches the next tick.
    #[test]
    fn a_failed_tick_takes_and_drops_every_external_family() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"note("c*4").serial(9600).osc(57121).midi("test-port")"#)
            .expect("the score evaluates");
        let mut producer = rustel_runtime::LiveFileProducer::unwatched(Duration::from_millis(2))
            .expect("producer");
        let clock = Cell::new(0.0f64);
        producer
            .step_unwatched_with_clock(&mut session, || clock.get(), 48_000, |_| true)
            .expect("the first window schedules");

        let failed = take_tick_intents(&mut session, true);
        assert!(failed.midi.is_empty(), "a failed tick sent MIDI");
        assert!(failed.osc.is_empty(), "a failed tick sent OSC");
        assert!(failed.serial.is_empty(), "a failed tick sent serial");
        assert!(
            session.take_pending_midi().is_empty()
                && session.take_pending_osc().is_empty()
                && session.take_pending_serial().is_empty(),
            "a failed tick left intents staged for the next tick"
        );

        clock.set(4.0);
        producer
            .step_unwatched_with_clock(&mut session, || clock.get(), 48_000, |_| true)
            .expect("a later window schedules");
        let passed = take_tick_intents(&mut session, false);
        assert!(!passed.midi.is_empty(), "a successful tick dropped MIDI");
        assert!(!passed.osc.is_empty(), "a successful tick dropped OSC");
        assert!(
            !passed.serial.is_empty(),
            "a successful tick dropped serial"
        );
    }
}

#[cfg(all(test, feature = "device-audio"))]
mod live_polyphony_tests {
    use super::*;

    #[test]
    fn live_polyphony_follows_accepted_evaluations_without_restarting_output() {
        let mut session = Session::with_config(SessionConfig::default().with_max_polyphony(192))
            .expect("session");
        let mut device = rustel_audio::LiveScalarDevice::start_output_with_options(
            Some(rustel_audio::SILENT_OUTPUT_NAME),
            session.generation(),
            live_output_options(&session, None),
        )
        .expect("silent output");
        sync_live_polyphony(&session, &device);
        assert_eq!(device.max_polyphony(), 192);
        let stream = device.stream_id();
        for voices in [4, 256, 24] {
            session
                .evaluate(&format!("setMaxPolyphony({voices}); s('sine')"))
                .expect("accepted score");
            sync_live_polyphony(&session, &device);
            assert_eq!(device.max_polyphony(), voices);
            assert_eq!(
                device.stream_id(),
                stream,
                "changing voices does not restart output"
            );
        }
        assert!(
            session
                .evaluate("setMaxPolyphony(1); throw new Error('rejected')")
                .is_err()
        );
        sync_live_polyphony(&session, &device);
        assert_eq!(
            device.max_polyphony(),
            24,
            "a rejected reload leaves the last good limit"
        );
        session
            .evaluate("s('sine')")
            .expect("accepted score without setter");
        sync_live_polyphony(&session, &device);
        assert_eq!(
            device.max_polyphony(),
            24,
            "a module setting survives an unrelated edit"
        );
        assert_eq!(device.stream_id(), stream);
        device.recycle_output().expect("recycle");
        sync_live_polyphony(&session, &device);
        assert_eq!(device.max_polyphony(), 24);
    }
}

#[cfg(test)]
mod live_output_contract_tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn update_checks_require_an_interactive_human_command() {
        let human = Cli::try_parse_from(["rustel", "check", "-e", "silence"]).unwrap();
        assert!(human.checks_for_updates(true, false));
        assert!(!human.checks_for_updates(false, false));
        assert!(!human.checks_for_updates(true, true));

        for args in [
            vec!["check", "-e", "silence", "--no-update-check"],
            vec!["check", "-e", "silence", "--quiet"],
            vec!["check", "-e", "silence", "--no-input"],
            vec!["check", "-e", "silence", "--json"],
            vec!["check", "-e", "silence", "-vvv"],
            vec!["play", "song.strudel", "--ui-events"],
            vec!["play", "song.strudel", "--score-events"],
            vec!["play", "song.strudel", "--follow"],
            vec!["replay", "set.rustel-session", "--score-events"],
            vec!["replay", "set.rustel-session", "--follow"],
            vec![
                "trace",
                "song.strudel",
                "--device-audio",
                "--watch",
                "--score-events",
            ],
            vec!["watch-code"],
            vec!["samples", "cache"],
            vec!["samples", "clear"],
            vec!["serve-samples"],
            vec!["completions", "bash"],
            vec!["config", "get", "check_updates"],
            vec!["config", "set", "check_updates", "false"],
        ] {
            let cli = Cli::try_parse_from(std::iter::once("rustel").chain(args.clone()))
                .unwrap_or_else(|error| panic!("{args:?}: {error}"));
            assert!(!cli.checks_for_updates(true, false), "{args:?}");
        }
        #[cfg(feature = "studio")]
        {
            let studio = Cli::try_parse_from(["rustel", "studio"]).unwrap();
            assert!(studio.checks_for_updates(true, false));
            for flag in ["--list-themes", "--probe-terminal", "--performance-events"] {
                let cli = Cli::try_parse_from(["rustel", "studio", flag]).unwrap();
                assert!(!cli.checks_for_updates(true, false), "{flag}");
            }
        }
    }

    fn fixture() -> Vec<(LiveDetail, serde_json::Value)> {
        vec![
            (
                LiveDetail::Verbose,
                serde_json::json!({
                    "session_recording": {
                        "path": "take.rustel-session",
                        "keeps": "installed-saves",
                    }
                }),
            ),
            (
                LiveDetail::Verbose,
                serde_json::json!({
                    "sample_warmup": {
                        "files": 4,
                        "waited_ms": 23,
                    }
                }),
            ),
            (
                LiveDetail::Essential,
                serde_json::json!({
                    "live": {
                        "status": "started",
                        "path": "song.strudel",
                        "device": "Built-in Output",
                        "sample_rate": 48_000,
                        "stream_id": 9,
                    }
                }),
            ),
            (
                LiveDetail::Debug,
                serde_json::json!({
                    "live_load": {
                        "producer_busy": 0.125,
                        "steps": 500,
                        "verdict": "healthy",
                    }
                }),
            ),
            (
                LiveDetail::Essential,
                serde_json::json!({
                    "reload": {
                        "target": "score",
                        "path": "song.strudel",
                        "status": "installed",
                        "generation_before": 1,
                        "generation_after": 2,
                    }
                }),
            ),
            (
                LiveDetail::Essential,
                serde_json::json!({
                    "reload": {
                        "target": "score",
                        "path": "song.strudel",
                        "status": "rejected",
                        "generation_before": 2,
                        "generation_after": 2,
                        "error_kind": "evaluation",
                        "message": "javascript: Expected `)`",
                    }
                }),
            ),
            (
                LiveDetail::Essential,
                serde_json::json!({
                    "live": {
                        "status": "stopped",
                        "outcome_kind": "cancelled",
                        "stream_id": 9,
                    }
                }),
            ),
        ]
    }

    /// `--json` selects structured live events for the play command,
    /// including watched scores.
    #[test]
    fn json_on_a_watched_score_emits_the_live_stream_as_json() {
        let cli = Cli::try_parse_from([
            product::COMMAND_NAME,
            "play",
            "foo.strudel",
            "--watch",
            "--json",
        ])
        .expect("a watched score in json");
        assert!(cli.wants_json(), "the flag is read off the play command");
        // What `run_play` builds from it: the hidden `--ui-events` is not
        // the only way to ask for a structured stream.
        let output = LiveOutput::new(cli.verbose, cli.play().ui_events, cli.wants_json());
        assert!(output.structured());
        let lines = rendered(output);
        assert!(!lines.is_empty(), "a structured run says something");
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|_| panic!("--json emitted a human line: {line}"));
        }

        // And without it the same run stays human, so the flag is what
        // decides rather than the verbosity that happens to be set.
        let plain = Cli::try_parse_from([product::COMMAND_NAME, "play", "foo.strudel", "--watch"])
            .expect("a watched score");
        assert!(!plain.wants_json());
        let output = LiveOutput::new(plain.verbose, plain.play().ui_events, plain.wants_json());
        assert!(!output.structured());
        assert_human(&rendered(output));
    }

    /// Playback and replay both accept `--json` and use the same live event stream.
    /// Test the parser so the accepted command forms stay consistent.
    #[test]
    fn a_replay_speaks_json_like_any_other_played_score() {
        let replay = Cli::try_parse_from([
            product::COMMAND_NAME,
            "replay",
            "set.rustel-session",
            "--json",
        ])
        .expect("a replay in json");
        assert!(replay.wants_json());
        let watched = Cli::try_parse_from([
            product::COMMAND_NAME,
            "play",
            "foo.strudel",
            "--watch",
            "--json",
        ])
        .expect("a watched score in json");
        assert!(watched.wants_json());
        // And neither is JSON without being asked.
        for args in [
            vec![product::COMMAND_NAME, "replay", "set.rustel-session"],
            vec![product::COMMAND_NAME, "play", "foo.strudel", "--watch"],
        ] {
            assert!(
                !Cli::try_parse_from(args).expect("plain").wants_json(),
                "human is what a person gets by default"
            );
        }
    }

    /// `--json` selects the output format; verbosity selects the event detail.
    /// The hidden live-UI protocol receives all events regardless of verbosity.
    #[test]
    fn json_picks_the_form_and_v_picks_the_volume() {
        let quiet = rendered(LiveOutput::new(0, false, true));
        let loud = rendered(LiveOutput::new(2, false, true));
        assert!(
            quiet.len() < loud.len(),
            "-vv says more than none: {} vs {}",
            quiet.len(),
            loud.len()
        );
        for line in quiet.iter().chain(&loud) {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|_| panic!("--json emitted a human line: {line}"));
        }
        // The same events either way; only the words change.
        assert_eq!(
            quiet.len(),
            rendered(LiveOutput::new(0, false, false)).len()
        );
        assert_eq!(loud.len(), rendered(LiveOutput::new(2, false, false)).len());

        // The protocol is not a reading and keeps everything.
        let protocol = rendered(LiveOutput::new(0, true, false));
        assert!(
            protocol.len() > quiet.len(),
            "--ui-events is the whole stream: {} vs {}",
            protocol.len(),
            quiet.len()
        );
        assert_eq!(protocol.len(), fixture().len());
        // And `-vvv` still means everything, being past the deepest detail.
        assert_eq!(
            rendered(LiveOutput::new(3, false, false)).len(),
            fixture().len()
        );
    }

    fn rendered(output: LiveOutput) -> Vec<String> {
        fixture()
            .iter()
            .filter_map(|(detail, event)| output.render(*detail, event))
            .collect()
    }

    fn assert_human(lines: &[String]) {
        for line in lines {
            assert!(
                serde_json::from_str::<serde_json::Value>(line).is_err(),
                "human verbosity emitted a JSON diagnostic: {line}"
            );
        }
    }

    #[test]
    fn compact_verbosity_counts_work_before_and_after_a_subcommand() {
        for (args, expected) in [
            (
                vec![product::COMMAND_NAME, "query", "-v", "-e", "pure(1)"],
                1,
            ),
            (
                vec![product::COMMAND_NAME, "query", "-e", "pure(1)", "-vv"],
                2,
            ),
            (
                vec![product::COMMAND_NAME, "query", "-vvv", "-e", "pure(1)"],
                3,
            ),
        ] {
            let parsed = Cli::try_parse_from(&args)
                .unwrap_or_else(|error| panic!("could not parse {args:?}: {error}"));
            assert_eq!(parsed.verbose, expected, "{args:?}");
            assert!(matches!(parsed.command, Command::Query { .. }));
        }

        for args in [
            vec![
                product::COMMAND_NAME,
                "-vv",
                "play",
                "song.strudel",
                "--watch",
            ],
            vec![
                product::COMMAND_NAME,
                "play",
                "song.strudel",
                "--watch",
                "-vv",
            ],
        ] {
            let parsed = Cli::try_parse_from(&args)
                .unwrap_or_else(|error| panic!("could not parse {args:?}: {error}"));
            assert_eq!(parsed.verbose, 2, "{args:?}");
            assert_eq!(parsed.play().file, Some(PathBuf::from("song.strudel")));
            assert!(parsed.play().watch);
            assert!(!parsed.play().announce_score);
        }

        let score_events = Cli::try_parse_from([
            product::COMMAND_NAME,
            "play",
            "song.strudel",
            "--watch",
            "--score-events",
        ])
        .expect("explicit score event transport");
        assert!(score_events.play().announce_score);

        let finite_live = Cli::try_parse_from([
            product::COMMAND_NAME,
            "play",
            "song.strudel",
            "--duration",
            "40",
        ])
        .expect("finite live playback");
        assert_eq!(finite_live.play().duration, Some(40.0));

        let help = Cli::command().render_long_help().to_string();
        // Phrase assertions match the whitespace-normalised help: clap rewraps
        // this text to the terminal width (or $COLUMNS), and a phrase split
        // across a soft wrap is still the same phrase.
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("-v, -vv, -vvv"), "{help}");
        assert!(flat.contains("per-second engine-pressure"), "{help}");
        let mut command = Cli::command();
        let play = command
            .find_subcommand_mut("play")
            .expect("the play subcommand");
        let help = play.render_long_help().to_string();
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        // The buffer knob is not a callback size everywhere: on Windows the
        // callback keeps the device period and the size is queued ahead.
        assert!(
            flat.contains("the callback size on macOS and Linux; on Windows (WASAPI shared mode) the callback stays at the 10 ms device period and this is the buffer queued ahead of it"),
            "{help}"
        );
        assert!(!flat.contains("frames per callback"), "{help}");
        assert!(help.contains("--score-events"), "{help}");
        #[cfg(feature = "studio")]
        {
            let studio = command
                .find_subcommand_mut("studio")
                .expect("the studio subcommand");
            let help = studio.render_long_help().to_string();
            let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                flat.contains("on Windows (WASAPI shared mode) the callback stays at the 10 ms device period and this is the buffer queued ahead of it"),
                "{help}"
            );
            assert!(!flat.contains("frames per callback"), "{help}");
        }
    }

    #[test]
    fn default_live_output_is_only_the_human_lifecycle_and_reload_result() {
        let lines = rendered(LiveOutput::new(0, false, false));
        assert_eq!(
            lines,
            [
                "Started song.strudel",
                "Updated song.strudel",
                "Update failed for song.strudel: javascript: Expected `)`",
                "Stopped",
            ]
        );
        assert_human(&lines);
    }

    #[test]
    fn v_and_vv_add_setup_then_performance_detail_without_switching_to_json() {
        let verbose = rendered(LiveOutput::new(1, false, false));
        assert_eq!(
            verbose,
            [
                "Recording session to take.rustel-session",
                "Prepared 4 sample files in 23 ms",
                "Started song.strudel on Built-in Output at 48000 Hz",
                "Updated song.strudel",
                "Update failed for song.strudel: javascript: Expected `)`",
                "Stopped",
            ]
        );
        assert_human(&verbose);

        let debug = rendered(LiveOutput::new(2, false, false));
        assert_eq!(
            debug,
            [
                "Recording session to take.rustel-session",
                "Prepared 4 sample files in 23 ms",
                "Started song.strudel on Built-in Output at 48000 Hz",
                "Load: healthy (12.5% producer)",
                "Updated song.strudel",
                "Update failed for song.strudel: javascript: Expected `)`",
                "Stopped",
            ]
        );
        assert_human(&debug);
    }

    #[test]
    fn versioned_engine_pressure_is_human_at_vv_and_structured_at_vvv() {
        let event = serde_json::json!({
            "live_load": {
                "producer_busy": 0.11,
                "steps": 256,
                "verdict": "healthy",
            },
            "engine_pressure": {
                "schema_version": 1,
                "status": {
                    "level": "normal",
                    "cause": "healthy",
                    "message": "healthy",
                },
                "dsp": { "slow_load_basis_points": 2_400 },
                "scheduler": {
                    "slow_load_basis_points": 1_100,
                    "cover_end_nanos": 420_000_000,
                },
                "voices": {
                    "active": 18,
                    "semantic_capacity": 128,
                },
            },
        });
        assert_eq!(
            LiveOutput::new(2, false, false).render(LiveDetail::Debug, &event),
            Some("Load: healthy (DSP 24%, scheduler 11%, voices 18/128, cover 420ms)".into())
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &LiveOutput::new(3, false, false)
                    .render(LiveDetail::Debug, &event)
                    .expect("structured pressure event")
            )
            .expect("pressure event JSON"),
            event
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn live_pressure_event_keeps_legacy_load_and_adds_the_v1_projection() {
        let mut pressure = rustel_runtime::EnginePressureSnapshot::default();
        pressure.device.stream_id = 42;
        pressure.device.realtime_load.publication = 1;
        pressure.device.realtime_load.sample_rate_hz = 48_000;
        pressure.device.realtime_pressure.publication = 1;
        pressure.producer.publication = 1;
        pressure.producer.slow_load_basis_points = 1_250;
        pressure.producer.window_turns = 64;
        let event = live_engine_pressure_event(
            pressure,
            rustel_runtime::EnginePressureReportContext {
                device_name: Some("silent".into()),
                requested_buffer_frames: Some(256),
                process_cpu_percent: Some(12.5),
                process_resident_bytes: Some(1_024),
                ..rustel_runtime::EnginePressureReportContext::default()
            },
        );
        assert_eq!(event["live_load"]["producer_busy"], 0.125);
        assert_eq!(event["live_load"]["steps"], 64);
        assert_eq!(event["engine_pressure"]["schema_version"], 1);
        assert_eq!(event["engine_pressure"]["device"]["name"], "silent");
        assert_eq!(
            event["engine_pressure"]["process"],
            serde_json::json!({ "cpu_percent": 12.5, "resident_bytes": 1_024 })
        );
    }

    #[test]
    fn vvv_and_ui_event_mode_preserve_every_legacy_json_record() {
        let verbose_json = rendered(LiveOutput::new(3, false, false));
        let ui_json = rendered(LiveOutput::new(0, true, false));
        assert_eq!(ui_json, verbose_json, "UI mode did not force JSON output");

        let actual = verbose_json
            .iter()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|error| {
                    panic!("legacy diagnostic is not JSON: {error}: {line}")
                })
            })
            .collect::<Vec<_>>();
        let expected = fixture()
            .into_iter()
            .map(|(_, event)| event)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn a_score_log_line_is_printed_as_itself() {
        let event = serde_json::json!({
            "live_error": {
                "kind": "log",
                "message": "[hap] 0/1 → 1/4: note:c s:sine",
                "recoverable": true,
            }
        });
        for verbosity in 0..=2 {
            assert_eq!(
                LiveOutput::new(verbosity, false, false).render(LiveDetail::Essential, &event),
                Some("[hap] 0/1 → 1/4: note:c s:sine".into()),
                "a log line carries no verdict of its own"
            );
        }
    }

    #[test]
    fn recoverable_compile_errors_are_never_hidden_at_human_levels() {
        let event = serde_json::json!({
            "live_error": {
                "kind": "evaluation",
                "message": "javascript: Unexpected token",
                "recoverable": true,
            }
        });
        for verbosity in 0..=2 {
            assert_eq!(
                LiveOutput::new(verbosity, false, false).render(LiveDetail::Essential, &event),
                Some("Error: javascript: Unexpected token".into())
            );
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &LiveOutput::new(3, false, false)
                    .render(LiveDetail::Essential, &event)
                    .expect("-vvv diagnostic")
            )
            .expect("-vvv compile error JSON"),
            event
        );
    }

    #[test]
    fn replay_metadata_is_human_until_full_json_verbosity() {
        let event = serde_json::json!({
            "replay": {
                "session": "take.rustel-session",
                "saves": 12,
                "duration_secs": 42.25,
            }
        });
        assert_eq!(
            LiveOutput::new(0, false, false).render(LiveDetail::Essential, &event),
            Some("Replaying take.rustel-session".into())
        );
        assert_eq!(
            LiveOutput::new(1, false, false).render(LiveDetail::Essential, &event),
            Some("Replaying take.rustel-session (12 saves, 42.2s)".into())
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &LiveOutput::new(3, false, false)
                    .render(LiveDetail::Essential, &event)
                    .expect("JSON replay metadata")
            )
            .expect("parse replay metadata"),
            event
        );
    }

    #[cfg(feature = "midi")]
    #[test]
    fn midi_summary_is_json_even_without_messages() {
        use rustel_midi::input::{InputSummary, MidiEvent};
        let mut summary = InputSummary::default();
        assert_eq!(midi_summary_json(&summary, 0.0)["midi_summary"]["total"], 0);
        summary.observe(MidiEvent::ControlChange {
            channel: 2,
            controller: 74,
            value: 42,
        });
        let report = midi_summary_json(&summary, 1.5);
        assert_eq!(report["midi_summary"]["total"], 1);
        assert_eq!(
            report["midi_summary"]["controls"][0],
            serde_json::json!([2, 74, 42, 42, 1])
        );
    }

    #[test]
    fn verbose_json_shorthand_is_limited_to_live_commands() {
        let cli = Cli::try_parse_from(["rustel", "devices", "-vvv"]).unwrap();
        assert!(!cli.wants_json());
        let cli = Cli::try_parse_from(["rustel", "devices", "-vvv", "--json"]).unwrap();
        assert!(cli.wants_json());
    }

    #[test]
    fn human_events_show_untrusted_controls_without_running_them() {
        let event = serde_json::json!({"replay": {"session": "set\u{1b}]52;c;payload\u{7}\u{202e}", "saves": 1}});
        let human = LiveOutput::new(0, false, false)
            .render(LiveDetail::Essential, &event)
            .unwrap();
        assert!(human.contains("set␛]52;c;payload␇�"), "{human}");
        assert!(!human.contains('\u{1b}'));
        assert!(!human.contains('\u{7}'));
        assert!(!human.contains('\u{202e}'));

        let structured = LiveOutput::new(0, false, true)
            .render(LiveDetail::Essential, &event)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&structured).unwrap(),
            event
        );
    }
}

#[cfg(all(test, feature = "device-audio"))]
mod tests {
    #[cfg(feature = "studio")]
    use super::*;
    use clap::CommandFactory;

    /// The human form prints what the stream opened as, once, at the default
    /// verbosity.
    #[test]
    fn the_stream_facts_have_a_human_line() {
        let event = serde_json::json!({
            "audio": "coreaudio · MacBook Pro Speakers · 48000 Hz · 128 frames · 2.7 ms"
        });
        assert_eq!(
            super::human_live_event(&event, 0).as_deref(),
            Some("Audio out coreaudio · MacBook Pro Speakers · 48000 Hz · 128 frames · 2.7 ms")
        );
    }

    /// A sound that is still decoding is waiting, not failing. It must not
    /// print as "Error:".
    #[test]
    fn a_loading_sound_reads_as_waiting_and_a_refused_one_still_reads_as_an_error() {
        let line = |kind: &str, message: &str| {
            super::human_live_event(
                &serde_json::json!({
                    "live_error": { "kind": kind, "message": message, "recoverable": true }
                }),
                0,
            )
        };
        assert_eq!(
            line(
                rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC,
                "sample \"hh:0\" is still loading"
            )
            .as_deref(),
            Some("waiting: sample \"hh:0\" is still loading")
        );
        assert_eq!(
            line(
                rustel_runtime::SAMPLE_AWAITED_DIAGNOSTIC,
                "sample \"hh:0\" is still loading"
            )
            .as_deref(),
            Some("waiting: sample \"hh:0\" is still loading")
        );
        assert_eq!(
            line("voice-refused", "unknown sound \"nosuch\"").as_deref(),
            Some("Error: unknown sound \"nosuch\"")
        );
        assert_eq!(
            line("sample-failed", "bd.wav: 404").as_deref(),
            Some("warning: bd.wav: 404 - skipped, the set plays on")
        );
        assert_eq!(
            line("serial-baud", "serialbaud 9600 was ignored").as_deref(),
            Some("warning: serialbaud 9600 was ignored")
        );
        // A slow serial port is waiting and a dropped write is a warning; only
        // a port that could not be opened is an error.
        assert_eq!(
            line("serial-opening", "serial port \"COM5\" is still opening").as_deref(),
            Some("waiting: serial port \"COM5\" is still opening")
        );
        assert_eq!(
            line("serial-trouble", "serial writes arrived too late").as_deref(),
            Some("warning: serial writes arrived too late")
        );
        assert_eq!(
            line("serial", "could not open serial port \"COM5\"").as_deref(),
            Some("Error: could not open serial port \"COM5\"")
        );
        // The kinds are the bridge's own, so the two cannot drift apart.
        #[cfg(feature = "serial")]
        {
            use rustel_runtime::serial_bridge::SerialNewsKind;
            assert_eq!(SerialNewsKind::StillOpening.live_kind(), "serial-opening");
            assert_eq!(SerialNewsKind::Trouble.live_kind(), "serial-trouble");
            assert_eq!(SerialNewsKind::Failure.live_kind(), "serial");
        }
    }

    /// A Windows console control event maps to the signal number used for the
    /// `128 + signal` exit. Only the closing events make the handler wait.
    #[cfg(windows)]
    #[test]
    fn every_console_stop_maps_to_a_signal_and_only_the_closing_ones_wait() {
        use windows_sys::Win32::System::Console::{
            CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT,
            CTRL_SHUTDOWN_EVENT,
        };

        // The interrupts: the familiar 130, and the process carries on so a
        // second press can be the one that quits.
        for event in [CTRL_C_EVENT, CTRL_BREAK_EVENT] {
            assert_eq!(super::console_event_signal(event), Some(2), "{event}");
            assert!(!super::console_event_closes(event), "{event}");
        }
        // Closing the window is this platform's SIGHUP, and the three that
        // end the process are exactly the three that must wait.
        assert_eq!(super::console_event_signal(CTRL_CLOSE_EVENT), Some(1));
        for event in [CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT] {
            assert_eq!(super::console_event_signal(event), Some(15), "{event}");
        }
        for event in [CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT] {
            assert!(super::console_event_closes(event), "{event}");
        }
        // Anything else belongs to whoever registered after us.
        assert_eq!(super::console_event_signal(9), None);
        assert!(!super::console_event_closes(9));

        // The exit codes `main` builds from these are the ones a caller
        // already knows from Unix.
        for (event, code) in [
            (CTRL_C_EVENT, 130u8),
            (CTRL_CLOSE_EVENT, 129),
            (CTRL_SHUTDOWN_EVENT, 143),
        ] {
            let signal = super::console_event_signal(event).expect("a stop");
            assert_eq!(128u8.saturating_add(signal as u8), code, "{event}");
        }
    }

    /// The stop notice prints at the default verbosity and before the wait.
    /// Without it, the tail wait of up to eight seconds looks like a hang.
    #[test]
    fn the_stop_says_it_is_ringing_out_and_how_to_skip_it() {
        let event = serde_json::json!({
            "stopping": { "message": "letting the tail ring out - Ctrl-C again to quit now" }
        });
        assert_eq!(
            super::human_live_event(&event, 0).as_deref(),
            Some("Playback stopped, letting the tail ring out - Ctrl-C again to quit now")
        );
    }

    #[cfg(feature = "midi")]
    #[test]
    fn midi_input_maintenance_prunes_unschedulable_candidate_generations() {
        let bus = rustel_core::midi_in::InputBus::new();
        let mut inputs = rustel_runtime::midi_input::MidiInputs::new();
        assert!(sync_live_midi_inputs(&mut inputs, &bus, 0, 0).is_empty());

        // More than the bus's unresolved-generation ceiling. Each generation
        // models an accepted save whose producer step then fails; live-loop
        // maintenance must still acknowledge it and retire the prior one.
        for generation in 1u64..=128 {
            bus.commit_generation(generation, Vec::new())
                .expect("maintenance should prevent candidate buildup");
            assert!(
                sync_live_midi_inputs(&mut inputs, &bus, 0, generation).is_empty(),
                "an empty candidate should not report a device error"
            );
        }
        assert_eq!(inputs.connected(), 0);
    }

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("rustel-bin-{name}-{}-{nonce}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn decoded_sample(value: f32) -> rustel_audio::DecodedSample {
        rustel_audio::DecodedSample::from_parts(48_000, 1, vec![value]).expect("decoded sample")
    }

    #[test]
    fn a_full_sample_install_ring_preserves_the_refused_item_and_its_tail() {
        let ids = [
            rustel_audio::SampleId(1),
            rustel_audio::SampleId(2),
            rustel_audio::SampleId(3),
        ];
        let ready = ids
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, decoded_sample(index as f32 + 1.0)))
            .collect();
        let mut retained = std::collections::HashMap::new();
        let mut accepted = Vec::new();

        let retry = install_sample_batch(ready, &mut retained, |id, decoded| {
            if accepted.len() == 1 {
                Err(decoded)
            } else {
                accepted.push(id);
                Ok(())
            }
        });

        assert_eq!(accepted, vec![ids[0]]);
        assert_eq!(retained.keys().copied().collect::<Vec<_>>(), vec![ids[0]]);
        assert_eq!(
            retry.into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
            vec![ids[1], ids[2]],
            "the refused sample and every unvisited sample must remain retryable"
        );
    }

    #[test]
    fn device_recycle_reinstalls_pcm_and_keeps_a_newer_publication_authoritative() {
        let retained_only_id = rustel_audio::SampleId(8);
        let replaced_id = rustel_audio::SampleId(9);
        let old = decoded_sample(0.25);
        let new = decoded_sample(0.75);
        let mut retained = std::collections::HashMap::from([
            (retained_only_id, old.clone()),
            (replaced_id, old.clone()),
        ]);
        // `requeue_retained_samples` and the library's atomic ordering are
        // exercised in library tests; this binary-level test pins the final
        // install/retention behavior without constructing a test-only library.
        let ready = vec![(retained_only_id, old.clone()), (replaced_id, new.clone())];

        let mut installed = Vec::new();
        let retry = install_sample_batch(ready, &mut retained, |id, decoded| {
            installed.push((id, decoded));
            Ok(())
        });

        assert!(retry.is_empty());
        assert_eq!(
            installed,
            vec![(retained_only_id, old), (replaced_id, new.clone())]
        );
        assert_eq!(retained.get(&replaced_id), Some(&new));
    }

    #[test]
    fn an_out_of_range_sample_id_is_never_retained_for_recycle() {
        let id = rustel_audio::SampleId(rustel_audio::SAMPLE_BANK_CAPACITY as u32);
        let mut retained = std::collections::HashMap::new();
        let retry =
            install_sample_batch(
                vec![(id, decoded_sample(0.5))],
                &mut retained,
                |_, _| Ok(()),
            );
        assert!(retry.is_empty());
        assert!(retained.is_empty());
    }

    #[test]
    fn internal_ui_event_flag_is_accepted_but_hidden_from_help() {
        let parsed = Cli::try_parse_from([
            product::COMMAND_NAME,
            "play",
            "score.strudel",
            "--watch",
            "--ui-events",
        ])
        .expect("internal UI flag");
        assert!(parsed.play().ui_events);

        let help = Cli::command()
            .find_subcommand_mut("play")
            .expect("the play subcommand")
            .render_long_help()
            .to_string();
        assert!(help.contains("--watch"), "play help omitted its options");
        assert!(!help.contains("--ui-events"));

        assert!(
            Cli::try_parse_from([
                product::COMMAND_NAME,
                "play",
                "score.strudel",
                "--watch",
                "--follow",
                "--ui-events",
            ])
            .is_err(),
            "human stdout rendering could corrupt the UI event stream"
        );
    }

    #[cfg(feature = "studio")]
    #[test]
    fn the_studio_refuses_a_prebake_flag_and_says_where_its_own_live() {
        // Accepted by the parser on purpose: clap's own "unexpected
        // argument" would send someone hunting for a file, when the answer
        // is a sheet inside the studio.
        let parsed = Cli::try_parse_from([
            product::COMMAND_NAME,
            "studio",
            "song.strudel",
            "--prebake",
            "setup.js",
        ])
        .expect("the flag parses so the refusal can explain itself");
        let Command::Studio { prebake, .. } = parsed.command else {
            panic!("expected the studio command");
        };
        assert_eq!(prebake.as_deref(), Some(std::path::Path::new("setup.js")));

        let error = run_command(
            Cli::try_parse_from([
                product::COMMAND_NAME,
                "studio",
                "song.strudel",
                "--prebake",
                "setup.js",
            ])
            .expect("parse")
            .command,
            0,
            rustel_audio::DspDispatch::automatic(),
            false,
        )
        .expect_err("the studio has no --prebake");
        let message = error.to_string();
        assert!(message.contains("^O"), "{message}");
        assert!(message.contains("rustel-set.json"), "{message}");
        assert_eq!(error.kind(), "invalid-argument");

        let help = Cli::try_parse_from([product::COMMAND_NAME, "studio", "--help"])
            .expect_err("help exits through clap")
            .to_string();
        assert!(!help.contains("--prebake"), "a refused flag was advertised");
    }

    #[cfg(feature = "studio")]
    #[test]
    fn internal_studio_performance_flag_is_accepted_but_hidden_from_help() {
        let parsed = Cli::try_parse_from([product::COMMAND_NAME, "studio", "--performance-events"])
            .expect("internal Studio performance flag");
        assert!(matches!(
            parsed.command,
            Command::Studio {
                performance_events: true,
                ..
            }
        ));

        let help = Cli::try_parse_from([product::COMMAND_NAME, "studio", "--help"])
            .expect_err("help exits through clap")
            .to_string();
        assert!(!help.contains("--performance-events"));
    }

    #[cfg(all(feature = "studio", not(feature = "remote-control")))]
    #[test]
    fn studio_has_no_remote_control_when_feature_is_omitted() {
        let help = Cli::try_parse_from([product::COMMAND_NAME, "studio", "--help"])
            .expect_err("help exits through clap")
            .to_string();
        assert!(
            !help.contains("--remote-control"),
            "a build without the feature advertised remote control"
        );
        assert!(
            Cli::try_parse_from([product::COMMAND_NAME, "studio", "--remote-control"]).is_err(),
            "a build without the feature accepted --remote-control"
        );
        assert!(!help.contains("--auth-token"));
    }

    #[cfg(all(feature = "studio", feature = "remote-control"))]
    #[test]
    fn studio_remote_control_is_off_unless_asked() {
        let absent = Cli::try_parse_from([product::COMMAND_NAME, "studio"])
            .expect("studio")
            .command;
        assert!(matches!(
            absent,
            Command::Studio {
                remote_control: None,
                auth_token: None,
                ..
            }
        ));

        let parsed = Cli::try_parse_from([product::COMMAND_NAME, "studio", "--remote-control"])
            .expect("default port")
            .command;
        assert!(matches!(
            parsed,
            Command::Studio {
                remote_control: Some(address),
                ..
            } if address == "127.0.0.1:9247".parse().unwrap()
        ));

        let chosen = Cli::try_parse_from([
            product::COMMAND_NAME,
            "studio",
            "song.strudel",
            "--remote-control=9000",
        ])
        .expect("chosen port")
        .command;
        match chosen {
            Command::Studio {
                file,
                remote_control,
                ..
            } => {
                assert_eq!(file.as_deref(), Some(std::path::Path::new("song.strudel")));
                assert_eq!(remote_control, Some("127.0.0.1:9000".parse().unwrap()));
            }
            other => panic!("expected the studio command, got {other:?}"),
        }

        for address in [
            "[::1]:9000",
            "0.0.0.0:9247",
            "[::]:9247",
            "192.168.1.10:8888",
        ] {
            let parsed = Cli::try_parse_from([
                product::COMMAND_NAME,
                "studio",
                &format!("--remote-control={address}"),
            ])
            .expect("explicit address")
            .command;
            assert!(matches!(
                parsed,
                Command::Studio {
                    remote_control: Some(bind),
                    ..
                } if bind == address.parse().unwrap()
            ));
        }

        for address in ["0", "studio.local:9000", ""] {
            assert!(
                Cli::try_parse_from([
                    product::COMMAND_NAME,
                    "studio",
                    &format!("--remote-control={address}"),
                ])
                .is_err(),
                "invalid address accepted: {address}"
            );
        }

        let token = "gig1";
        let with_token = Cli::try_parse_from([
            product::COMMAND_NAME,
            "studio",
            "--remote-control=192.168.1.10:8888",
            "--auth-token",
            token,
        ])
        .expect("explicit token")
        .command;
        assert!(matches!(
            with_token,
            Command::Studio {
                remote_control: Some(_),
                auth_token: Some(value),
                ..
            } if value == token
        ));
        for arguments in [
            vec!["--auth-token", token],
            vec!["--remote-control", "--auth-token", "abc"],
        ] {
            assert!(
                Cli::try_parse_from(
                    [product::COMMAND_NAME, "studio"]
                        .into_iter()
                        .chain(arguments),
                )
                .is_err()
            );
        }

        let help = Cli::try_parse_from([product::COMMAND_NAME, "studio", "--help"])
            .expect_err("help exits through clap")
            .to_string();
        assert!(help.contains("--remote-control"));
        assert!(help.contains("--auth-token"));
    }

    #[test]
    fn ui_layout_delivery_retries_owned_snapshot_once_per_generation() {
        let mut delivery = UiLayoutDelivery::default();
        assert!(delivery.observe("note(\"c4\").scope()", 7).expect("layout"));
        assert!(!delivery.ready_for(7));
        assert!(delivery.audio_requested());

        let mut attempts = 0;
        assert!(!delivery.try_deliver(|layout| {
            attempts += 1;
            Err(layout)
        }));
        assert_eq!(attempts, 1);
        assert!(
            !delivery
                .observe("this source is not rebuilt", 7)
                .expect("same generation")
        );

        let mut delivered = None;
        assert!(delivery.try_deliver(|layout| {
            attempts += 1;
            delivered = Some(layout.ui_layout.generation);
            Ok(())
        }));
        assert_eq!(attempts, 2);
        assert_eq!(delivered, Some(7));
        assert!(delivery.ready_for(7));
        assert!(!delivery.try_deliver(|_| panic!("layout emitted twice")));

        assert!(
            delivery
                .observe("note(\"e4\")._pianoroll()", 8)
                .expect("next layout")
        );
        assert!(!delivery.ready_for(8));
        assert!(!delivery.audio_requested());
    }

    #[test]
    fn ui_layout_stays_on_the_audible_generation_until_cutover() {
        let mut delivery = UiLayoutDelivery::default();
        let old_source = "note(\"c4\").scope()";
        let candidate = "note(\"d4\").pianoroll()";
        assert!(delivery.observe(old_source, 7).expect("old layout"));
        assert!(delivery.try_deliver(|_| Ok(())));
        assert!(delivery.ready_for(7));

        assert!(
            !observe_ui_layout_if_audible(&mut delivery, candidate, 8, 7)
                .expect("unpublished candidate")
        );
        assert_eq!(delivery.observed_generation, Some(7));
        assert!(delivery.ready_for(7));
        assert!(delivery.audio_requested());

        assert!(
            observe_ui_layout_if_audible(&mut delivery, candidate, 8, 8)
                .expect("completed cutover")
        );
        assert_eq!(delivery.observed_generation, Some(8));
        assert!(!delivery.ready_for(8));
        assert!(!delivery.audio_requested());
    }

    #[test]
    fn ui_visual_audio_captures_only_pattern_audio_slots() {
        let mut delivery = UiLayoutDelivery::default();
        delivery
            .observe(
                "$: note(\"c4\")._scope()\n$: note(\"e4\")._pianoroll()\n$: note(\"g4\")._spectrum()\n$: note(\"b4\").tscope()\nall(spectrum)",
                7,
            )
            .expect("layout");
        assert!(delivery.audio_requested());
        assert_eq!(delivery.visual_audio_mask, 0b1101);

        delivery.observe("all(scope)", 8).expect("master scope");
        assert!(delivery.audio_requested());
        assert_eq!(delivery.visual_audio_mask, 0);

        delivery
            .observe("note(\"c4\")._pianoroll()", 9)
            .expect("event visual");
        assert!(!delivery.audio_requested());
        assert_eq!(delivery.visual_audio_mask, 0);
    }

    #[test]
    fn ui_visual_audio_resets_reassigned_slots_after_audible_layout_delivery() {
        use live::{UiVisualAudioCapture, UiVisualAudioUpdate};

        let mut delivery = UiLayoutDelivery::default();
        let mut capture = UiVisualAudioCapture::default();
        let source = "note(\"c4\")._scope()";
        delivery.observe(source, 7).expect("layout");
        assert_eq!(capture.update(&delivery, 7), None);
        assert!(!delivery.try_deliver(Err));
        assert_eq!(capture.update(&delivery, 7), None);
        assert!(delivery.try_deliver(|_| Ok(())));
        assert_eq!(
            capture.update(&delivery, 7),
            Some(UiVisualAudioUpdate::Reset(1))
        );
        assert_eq!(capture.update(&delivery, 7), None);

        // Re-querying the same source preserves its slot membership and tails.
        delivery.observe(source, 8).expect("same source");
        assert_eq!(
            capture.update(&delivery, 8),
            Some(UiVisualAudioUpdate::Set(0))
        );
        assert!(delivery.try_deliver(|_| Ok(())));
        assert_eq!(
            capture.update(&delivery, 8),
            Some(UiVisualAudioUpdate::Set(1))
        );
        assert_eq!(capture.update(&delivery, 8), None);

        let replacement = "note(\"g5\")._spectrum()";
        assert!(!observe_ui_layout_if_audible(&mut delivery, replacement, 9, 8).unwrap());
        assert_eq!(capture.update(&delivery, 8), None);
        assert!(observe_ui_layout_if_audible(&mut delivery, replacement, 9, 9).unwrap());
        assert_eq!(
            capture.update(&delivery, 9),
            Some(UiVisualAudioUpdate::Set(0))
        );
        assert!(!delivery.try_deliver(Err));
        assert_eq!(capture.update(&delivery, 9), None);
        assert!(delivery.try_deliver(|_| Ok(())));
        assert_eq!(
            capture.update(&delivery, 9),
            Some(UiVisualAudioUpdate::Reset(1))
        );

        delivery
            .observe("note(\"a4\")", 10)
            .expect("removed visual");
        assert!(delivery.try_deliver(|_| Ok(())));
        assert_eq!(
            capture.update(&delivery, 10),
            Some(UiVisualAudioUpdate::Reset(0))
        );
        assert_eq!(capture.update(&delivery, 10), None);
    }

    #[test]
    fn ui_visual_audio_emits_distinct_lane_spectra_and_the_master_mix() {
        use rustel_audio::{AudioEvent, device::ManualLiveOutput};
        use rustel_runtime::{
            ui_analysis::UiAudioAnalyzer,
            ui_events::{UiAudioEnvelope, UiEventSink},
        };

        let mut output = ManualLiveOutput::new(48_000, 1).expect("silent output");
        output.device().set_analysis_enabled(true);
        output.device().set_visual_analysis_mask(0b11);
        for (onset_id, freq_hz, ui_visuals) in [(1, 220.0, 0b01), (2, 1760.0, 0b10)] {
            assert!(output.device().push(AudioEvent {
                onset_id,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz,
                gain: 0.25,
                duration_secs: 1.0,
                ui_visuals,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
        }
        let mut block = [0.0; 128 * 2];
        for _ in 0..64 {
            output.render(&mut block);
        }

        let captured = tempfile::NamedTempFile::new().expect("audio output");
        let sink = UiEventSink::with_writer(1, captured.reopen().expect("writer"))
            .expect("audio event sink");
        let mut sequence = 0;
        live::emit_ui_audio_frame(
            &sink,
            output.device(),
            1,
            0b11,
            &mut UiAudioAnalyzer::new(),
            &mut [0.0; rustel_audio::LIVE_ANALYSIS_WINDOW_SAMPLES],
            &mut sequence,
        );
        sink.finish().expect("writer finished");
        let envelope: UiAudioEnvelope =
            serde_json::from_reader(captured.reopen().expect("reader")).expect("audio JSON");
        envelope.validate().expect("valid audio frame");
        let frame = envelope.ui_audio;
        assert_eq!(frame.sequence, 1);
        assert_eq!(frame.generation, 1);
        assert_eq!(frame.sample_rate, 48_000);
        let visuals = frame.visuals.expect("per-visual audio");
        assert_eq!(
            visuals.iter().map(|visual| visual.slot).collect::<Vec<_>>(),
            [0, 1]
        );

        let peak = |spectrum: &[f32]| {
            spectrum
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .expect("spectral peak")
                .0
        };
        let low = peak(&visuals[0].spectrum);
        let high = peak(&visuals[1].spectrum);
        assert!((3..=5).contains(&low), "220 Hz peak: {low}");
        assert!((36..=38).contains(&high), "1760 Hz peak: {high}");
        assert!(visuals[0].spectrum[low] > visuals[0].spectrum[high] + 40.0);
        assert!(visuals[1].spectrum[high] > visuals[1].spectrum[low] + 40.0);
        assert!(frame.spectrum[low] > -30.0);
        assert!(frame.spectrum[high] > -30.0);
        for ((master, low), high) in frame
            .scope
            .iter()
            .zip(&visuals[0].scope)
            .zip(&visuals[1].scope)
        {
            assert!((master - low - high).abs() < 1e-5);
        }
    }

    #[test]
    #[cfg(any(feature = "midi", feature = "osc", feature = "serial"))]
    fn external_deadline_uses_the_remaining_device_clock_delay() {
        assert_eq!(
            remaining_output_delay(12.5, 10.0),
            std::time::Duration::from_secs_f64(2.5)
        );
        assert_eq!(
            remaining_output_delay(12.5, 11.75),
            std::time::Duration::from_secs_f64(0.75)
        );
        assert_eq!(
            remaining_output_delay(12.5, 13.0),
            std::time::Duration::ZERO
        );
    }

    fn slider_control(
        slider: &rustel_runtime::ui_events::UiSlider,
        layout: &UiLayoutDelivery,
        generation: u64,
        value: f64,
    ) -> UiSliderControl {
        UiSliderControl {
            version: UI_CONTROL_PROTOCOL_VERSION,
            kind: "slider".into(),
            generation,
            source_revision: layout.source_revision.clone().expect("layout revision"),
            id: slider.id.clone(),
            value,
        }
    }

    #[test]
    fn ui_control_reader_is_bounded_and_coalesces_latest_slider_value() {
        let revision = "a".repeat(64);
        let record = |id: &str, value: f64| {
            serde_json::json!({
                "ui_control": {
                    "version": 1,
                    "kind": "slider",
                    "generation": 7,
                    "source_revision": revision,
                    "id": id,
                    "value": value,
                }
            })
            .to_string()
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(record("10:13", 0.1).as_bytes());
        bytes.push(b'\n');
        bytes.extend(std::iter::repeat_n(b'x', MAX_UI_CONTROL_LINE_BYTES + 1));
        bytes.push(b'\n');
        bytes.extend_from_slice(b"{not-json}\n");
        bytes.extend_from_slice(record("10:13", 0.9).as_bytes());
        bytes.push(b'\n');
        bytes.extend_from_slice(record("20:23", 0.4).as_bytes());
        bytes.push(b'\n');

        let pending = std::sync::Mutex::new(std::collections::BTreeMap::new());
        read_ui_controls(std::io::Cursor::new(bytes), &pending);
        let pending = pending.into_inner().expect("pending controls");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending["10:13"].value, 0.9);
        assert_eq!(pending["20:23"].value, 0.4);

        let mut flood = Vec::new();
        for index in 0..MAX_PENDING_UI_SLIDERS + 8 {
            flood.extend_from_slice(record(&format!("{index}:{}", index + 1), 0.5).as_bytes());
            flood.push(b'\n');
        }
        let bounded = std::sync::Mutex::new(std::collections::BTreeMap::new());
        read_ui_controls(std::io::Cursor::new(flood), &bounded);
        assert_eq!(
            bounded.into_inner().expect("bounded controls").len(),
            MAX_PENDING_UI_SLIDERS
        );
    }

    #[test]
    fn slider_control_applies_only_to_exact_active_layout_and_range() {
        let source = "slider(.25, 0, 1, .05)";
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("slider score");
        let generation = session.generation();
        let mut layout = UiLayoutDelivery::default();
        layout.observe(source, generation).expect("slider layout");

        // A control naming a layout that was built but never delivered cannot
        // come from a client that follows the protocol.
        let undelivered = slider_control(
            layout.sliders.values().next().expect("layout slider"),
            &layout,
            generation,
            0.75,
        );
        assert_eq!(
            apply_ui_slider_control(&session, &mut layout, &undelivered, None),
            UiSliderApplyStatus::Stale
        );
        assert!(layout.try_deliver(|_| Ok(())));

        let applied = slider_control(&layout.sliders[&undelivered.id], &layout, generation, 0.75);
        assert!(applied.wire_valid());
        assert_eq!(
            apply_ui_slider_control(&session, &mut layout, &applied, None),
            UiSliderApplyStatus::Applied
        );
        assert_eq!(layout.sliders[&undelivered.id].value, 0.75);
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query updated slider");
        assert_eq!(haps[0].value.as_f64(), Some(0.75));

        let mut stale_generation = applied.clone();
        stale_generation.generation += 1;
        assert_eq!(
            apply_ui_slider_control(&session, &mut layout, &stale_generation, None),
            UiSliderApplyStatus::Stale
        );
        let mut stale_revision = applied.clone();
        stale_revision.source_revision = "b".repeat(64);
        assert_eq!(
            apply_ui_slider_control(&session, &mut layout, &stale_revision, None),
            UiSliderApplyStatus::Stale
        );
        let mut unknown = applied.clone();
        unknown.id = "999:1000".into();
        assert_eq!(
            apply_ui_slider_control(&session, &mut layout, &unknown, None),
            UiSliderApplyStatus::Unknown
        );
        let mut out_of_range = applied;
        out_of_range.value = 1.01;
        assert_eq!(
            apply_ui_slider_control(&session, &mut layout, &out_of_range, None),
            UiSliderApplyStatus::OutOfRange
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query retained slider")[0]
                .value
                .as_f64(),
            Some(0.75)
        );
    }

    #[test]
    fn slider_requery_replaces_only_events_at_and_after_the_safe_takeover() {
        let source = "slider(.25, 0, 1, .05).fast(8)";
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("slider score");
        session.set_schedule_lead(0.1);
        session.set_continuity_margin(0.1);
        let generation_before = session.generation();
        let old = session
            .schedule_through(0.0, 0.5)
            .expect("prefill old slider value");
        assert!(old.iter().any(|onset| onset.target_time < 0.2));
        assert!(old.iter().all(|onset| {
            onset.generation == generation_before
                && onset.value == rustel_runtime::ValueJson::Number(0.25)
        }));

        let mut layout = UiLayoutDelivery::default();
        layout
            .observe(source, generation_before)
            .expect("slider layout");
        assert!(layout.try_deliver(|_| Ok(())), "layout delivered");
        let slider = layout.sliders.values().next().expect("slider").clone();
        let control = slider_control(&slider, &layout, generation_before, 0.75);
        assert_eq!(
            apply_ui_slider_control(&session, &mut layout, &control, None),
            UiSliderApplyStatus::Applied
        );
        let (requery_before, generation_after) = session
            .requery_active_at(0.1)
            .expect("valid control requery")
            .expect("active graph");
        assert_eq!(requery_before, generation_before);
        assert_eq!(generation_after, generation_before + 1);
        assert_eq!(session.take_requery_takeover_time(), Some(0.2));

        assert!(
            observe_ui_layout_if_audible(&mut layout, source, generation_after, generation_after,)
                .expect("post-control layout")
        );
        layout.sync_slider_values(&session);
        assert_eq!(layout.sliders[&slider.id].value, 0.75);
        assert_eq!(
            layout
                .pending
                .as_ref()
                .expect("pending layout")
                .ui_layout
                .sliders[0]
                .value,
            0.75
        );

        let new = session
            .schedule_through(0.1, 0.6)
            .expect("prefill new slider value");
        assert!(!new.is_empty());
        assert!(new.iter().all(|onset| {
            onset.generation == generation_after
                && onset.target_time >= 0.2 - 1e-9
                && onset.value == rustel_runtime::ValueJson::Number(0.75)
        }));
    }

    #[test]
    fn unwatched_slider_control_cutover_publishes_the_device_generation() {
        use rustel_runtime::WatchLanguage;
        use std::sync::atomic::{AtomicU64, Ordering};

        // The slider rides a SOUNDING pattern. A bare `slider(...)` lane
        // haps as plain numbers, and strudel.cc refuses non-object hap
        // values, so such a score is silent on strudel.cc too -- every
        // onset would be refused and the cutover would have no audio to
        // prefill. This test is about the control cutover publishing a
        // generation, not about that refusal.
        let source = "s(\"bd*8\").speed(slider(.25, 0, 1, .05))";
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("slider score");
        session.set_schedule_lead(0.1);
        session.set_continuity_margin(0.1);
        let audible_generation = session.generation();
        let mut producer =
            rustel_runtime::LiveFileProducer::from_loaded_sources_with_prebake_floor(
                "unused.strudel",
                WatchLanguage::JavaScript,
                source,
                None,
                std::time::Duration::ZERO,
                std::time::Duration::from_millis(2),
                std::time::Duration::from_millis(2),
            )
            .expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation does not cut over"),
                |_| true,
            )
            .expect("initial prefill");

        let mut layout = UiLayoutDelivery::default();
        layout
            .observe(source, audible_generation)
            .expect("slider layout");
        // Controls are only legitimate for a layout the client received.
        assert!(layout.try_deliver(|_| Ok(())), "layout delivered");
        let slider = layout.sliders.values().next().expect("slider").clone();
        let control = slider_control(&slider, &layout, audible_generation, 0.75);
        assert_eq!(
            apply_ui_slider_control(&session, &mut layout, &control, None),
            UiSliderApplyStatus::Applied
        );
        let (before, after) = session
            .requery_active_at(0.0)
            .expect("control requery")
            .expect("active graph");
        producer.arm_control_requery(before, after);

        let published = AtomicU64::new(0);
        let takeover = AtomicU64::new(u64::MAX);
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| {
                    published.store(generation, Ordering::Release);
                    takeover.store(frame, Ordering::Release);
                },
                |_| true,
            )
            .expect("control cutover prefill");
        assert_eq!(published.load(Ordering::Acquire), after);
        assert_ne!(
            takeover.load(Ordering::Acquire),
            u64::MAX,
            "control cutover must publish a takeover frame with the generation"
        );
    }

    #[test]
    fn output_recycle_requeries_from_new_rate_lead_and_refills_before_cutover() {
        use rustel_runtime::WatchLanguage;

        let source = "note('c4').fast(16)";
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("score");
        session.set_schedule_lead(0.1);
        session.set_continuity_margin(0.1);
        let audible_generation = session.generation();
        let mut producer =
            rustel_runtime::LiveFileProducer::from_loaded_sources_with_prebake_floor(
                "unused.strudel",
                WatchLanguage::JavaScript,
                source,
                None,
                std::time::Duration::ZERO,
                std::time::Duration::from_millis(2),
                std::time::Duration::from_millis(2),
            )
            .expect("producer");
        let mut old_frames = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation does not cut over"),
                |event| {
                    old_frames.push(event.target_frame);
                    true
                },
            )
            .expect("old-rate prefill");
        assert!(
            !old_frames.is_empty(),
            "fixture produced no old-rate horizon"
        );
        assert_eq!(session.generation(), audible_generation);

        // The replacement stream runs at a different rate and has an empty
        // ring. Recovery skips only its newly consumed scheduling lead, then
        // queries the remainder of the normal live horizon in the new frame
        // domain before publishing the generation.
        let recovery_now = 0.25;
        let new_sample_rate = 44_100;
        let (before, after) = session
            .requery_after_output_recycle_at(recovery_now)
            .expect("recovery requery")
            .expect("active graph");
        assert_eq!(before, audible_generation);
        producer.arm_output_recovery_requery(before, after);

        let mut publication = None;
        let mut recovered_frames = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || recovery_now,
                new_sample_rate,
                |generation, takeover_frame, _cut| publication = Some((generation, takeover_frame)),
                |event| {
                    recovered_frames.push((event.generation, event.target_frame));
                    true
                },
            )
            .expect("new-rate recovery prefill");

        let expected_takeover = ((recovery_now + 0.1) * f64::from(new_sample_rate)).round() as u64;
        assert_eq!(publication, Some((after, expected_takeover)));
        assert!(
            !recovered_frames.is_empty(),
            "recovery published without refilling the drained horizon"
        );
        assert!(
            recovered_frames
                .iter()
                .all(|(generation, frame)| { *generation == after && *frame >= expected_takeover })
        );
    }

    #[test]
    fn failed_slider_prefill_can_be_corrected_from_the_audible_layout() {
        use rustel_runtime::WatchLanguage;

        let source = r#"note("c4").fast(slider(8, 1, 10000000, 1))"#;
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("slider score");
        session.set_schedule_lead(0.1);
        session.set_continuity_margin(0.1);
        let audible_generation = session.generation();
        let mut producer =
            rustel_runtime::LiveFileProducer::from_loaded_sources_with_prebake_floor(
                "unused.strudel",
                WatchLanguage::JavaScript,
                source,
                None,
                std::time::Duration::ZERO,
                std::time::Duration::from_millis(2),
                std::time::Duration::from_millis(2),
            )
            .expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation does not cut over"),
                |_| true,
            )
            .expect("initial prefill");

        let mut layout = UiLayoutDelivery::default();
        layout
            .observe(source, audible_generation)
            .expect("slider layout");
        assert!(layout.try_deliver(|_| Ok(())), "layout delivered");
        let slider = layout.sliders.values().next().expect("slider").clone();
        let bad = slider_control(&slider, &layout, audible_generation, 10_000_000.0);
        assert_eq!(
            apply_ui_slider_control(&session, &mut layout, &bad, None),
            UiSliderApplyStatus::Applied
        );
        let (before_bad, bad_generation) = session
            .requery_active_at(0.0)
            .expect("bad control requery")
            .expect("active graph");
        producer.arm_control_requery(before_bad, bad_generation);
        let pending = PendingUiControlCutover {
            audible_generation,
            session_generation: bad_generation,
        };

        let error = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("bad slider generation reached the device"),
                |_| true,
            )
            .expect_err("pathological slider value must refuse its prefill");
        assert!(
            matches!(error, RuntimeError::ResourceLimit(_)),
            "unexpected bad-slider error: {error:?}"
        );
        assert_eq!(session.active_source(), Some(source));

        let correction = slider_control(&slider, &layout, audible_generation, 8.0);
        let corrective_generation =
            pending.corrective_generation(session.generation(), audible_generation);
        assert_eq!(corrective_generation, Some(audible_generation));
        assert_eq!(
            apply_ui_slider_control(&session, &mut layout, &correction, corrective_generation,),
            UiSliderApplyStatus::Applied
        );
        let (before_good, good_generation) = session
            .requery_active_at(0.0)
            .expect("corrective requery")
            .expect("active graph");
        assert_eq!(before_good, bad_generation);
        producer.arm_control_requery(before_good, good_generation);

        let mut published = Vec::new();
        let recovered = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, _, _| published.push(generation),
                |_| true,
            )
            .expect("corrective value must supersede failed control prefill");
        assert_eq!(published, [good_generation]);
        assert_eq!(recovered.watch, rustel_runtime::WatchPoll::Unchanged);
        assert_eq!(session.active_source(), Some(source));
    }

    #[test]
    fn live_voice_exhaustion_is_a_resource_limit_not_a_device_failure() {
        let error = runtime_device_error(rustel_audio::DevicePlaybackError::ResourceLimit(
            "voice capacity".into(),
        ));
        assert_eq!(error.kind(), "resource-limit");
        assert_eq!(exit_code_for(error.kind()), exit::RESOURCE_LIMIT);
    }

    #[test]
    fn explicit_prebake_reaches_the_live_producer() {
        use rustel_runtime::{ReloadStatus, WatchLanguage, WatchPoll, WatchTarget};

        let temp = TempDir::new("live-prebake-assembly");
        let score = temp.0.join("song.strudel");
        let prebake = temp.0.join("prebake.js");
        let initial_setup = "globalThis.liveHelper = () => note('c4').fast(8);";
        let initial_score = "liveHelper()";
        std::fs::write(&prebake, initial_setup).expect("write setup");
        std::fs::write(&score, initial_score).expect("write score");
        let input = SourceInput {
            file: Some(score.clone()),
            eval: None,
            sample_access: SampleAccessArgs::default(),
        };
        let mut session = Session::new().expect("session");
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        let loaded = load_musician_sources(&mut session, &input, Some(&prebake), &cancellation)
            .expect("load product sources");
        let generation = session.generation();
        let mut producer = build_live_producer(
            &score,
            WatchLanguage::JavaScript,
            &loaded,
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(2),
        )
        .expect("assemble product producer");
        producer
            .step(
                &mut session,
                std::time::Duration::ZERO,
                0.0,
                48_000,
                |_, _, _| {},
                |_| true,
            )
            .expect("baseline step");

        std::fs::write(
            &prebake,
            "globalThis.liveHelper = () => note('e4').fast(8);",
        )
        .expect("edit setup");
        let pending = producer
            .step(
                &mut session,
                std::time::Duration::from_millis(20),
                0.020,
                48_000,
                |_, _, _| panic!("pending setup changed generation"),
                |_| true,
            )
            .expect("observe setup");
        assert_eq!(pending.prebake_watch, WatchPoll::Pending);
        let installed = producer
            .step(
                &mut session,
                std::time::Duration::from_millis(120),
                0.120,
                48_000,
                |_, _, _| panic!("setup-only edit changed generation"),
                |_| true,
            )
            .expect("install setup");
        let WatchPoll::Event(ref event) = installed.prebake_watch else {
            panic!("product setup was not watched: {installed:?}");
        };
        assert_eq!(event.target, WatchTarget::Prebake);
        assert_eq!(event.status, ReloadStatus::Installed);
        assert_eq!(session.generation(), generation);

        std::fs::write(&score, "liveHelper().fast(2)").expect("edit score");
        let pending = producer
            .step(
                &mut session,
                std::time::Duration::from_millis(140),
                0.140,
                48_000,
                |_, _, _| panic!("pending score changed generation"),
                |_| true,
            )
            .expect("observe score");
        assert_eq!(pending.watch, WatchPoll::Pending);
        let mut published = Vec::new();
        let installed = producer
            .step(
                &mut session,
                std::time::Duration::from_millis(240),
                0.240,
                48_000,
                |next, _, _| published.push(next),
                |_| true,
            )
            .expect("install score");
        let WatchPoll::Event(ref event) = installed.watch else {
            panic!("product score was not watched: {installed:?}");
        };
        assert_eq!(event.target, WatchTarget::Score(WatchLanguage::JavaScript));
        assert_eq!(event.status, ReloadStatus::Installed);
        assert_eq!(published, [generation + 1]);
        let current = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query product replacement");
        assert!(!current.is_empty(), "product replacement became silence");
        assert!(
            current.iter().all(|hap| hap.value.show().contains("e4")),
            "product dropped the explicit prebake: {current:?}"
        );
    }

    #[test]
    fn watched_ui_layout_recovers_from_an_invalid_startup_score() {
        use rustel_runtime::{ReloadStatus, WatchLanguage, WatchPoll};

        let temp = TempDir::new("ui-invalid-startup");
        let score = temp.0.join("song.strudel");
        std::fs::write(&score, "note(").expect("write invalid startup score");
        let input = SourceInput {
            file: Some(score.clone()),
            eval: None,
            sample_access: SampleAccessArgs::default(),
        };
        let mut session = Session::new().expect("session");
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        let (loaded, startup_error) =
            load_watch_musician_sources(&mut session, &input, None, &cancellation)
                .expect("watch startup remains recoverable");
        assert!(startup_error.is_some(), "invalid startup was not reported");
        assert!(
            session.active_source().is_none(),
            "invalid startup installed a source"
        );

        let mut producer = build_live_producer(
            &score,
            WatchLanguage::JavaScript,
            &loaded,
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(2),
        )
        .expect("assemble watched producer");
        let valid = r#"note("c4").scope()"#;
        std::fs::write(&score, valid).expect("repair startup score");
        let pending = producer
            .step(
                &mut session,
                std::time::Duration::from_millis(20),
                0.020,
                48_000,
                |_, _, _| panic!("pending repair changed generation"),
                |_| true,
            )
            .expect("observe repaired score");
        assert_eq!(pending.watch, WatchPoll::Pending);

        let continued = producer
            .step(
                &mut session,
                std::time::Duration::from_millis(21),
                0.021,
                48_000,
                |_, _, _| panic!("continuation sample changed generation"),
                |_| true,
            )
            .expect("measure no-pattern continuation");
        assert_eq!(continued.watch, WatchPoll::Unchanged);

        let installed = producer
            .step(
                &mut session,
                std::time::Duration::from_millis(140),
                0.140,
                48_000,
                |_, _, _| {},
                |_| true,
            )
            .expect("install repaired score");
        let WatchPoll::Event(event) = installed.watch else {
            panic!("repaired score did not produce a reload event: {installed:?}");
        };
        assert_eq!(event.status, ReloadStatus::Installed);
        assert_eq!(session.active_source(), Some(valid));

        let mut delivery = UiLayoutDelivery::default();
        assert!(
            delivery
                .observe(valid, session.generation())
                .expect("build recovered layout")
        );
        assert!(delivery.audio_requested());
        assert!(delivery.try_deliver(|layout| {
            assert_eq!(layout.ui_layout.generation, session.generation());
            assert_eq!(
                layout.ui_layout.source_revision,
                rustel_runtime::ui_events::source_revision(valid)
            );
            Ok(())
        }));
        assert!(delivery.ready_for(session.generation()));
    }

    /// A bounce's length as a person writes it: seconds, minutes, m:ss,
    /// hours, bars; nonsense is refused with the forms spelled out.
    #[test]
    fn render_lengths_read_seconds_minutes_and_bars() {
        assert_eq!(
            parse_render_length("60").unwrap(),
            RenderLength::Seconds(60.0)
        );
        assert_eq!(
            parse_render_length("30s").unwrap(),
            RenderLength::Seconds(30.0)
        );
        assert_eq!(
            parse_render_length("2m").unwrap(),
            RenderLength::Seconds(120.0)
        );
        assert_eq!(
            parse_render_length("1:30").unwrap(),
            RenderLength::Seconds(90.0)
        );
        assert_eq!(
            parse_render_length("1h").unwrap(),
            RenderLength::Seconds(3600.0)
        );
        assert_eq!(
            parse_render_length("16b").unwrap(),
            RenderLength::Cycles(16.0)
        );
        assert_eq!(
            parse_render_length(" 4 bars ").unwrap(),
            RenderLength::Cycles(4.0)
        );
        assert_eq!(
            parse_render_length("0.5m").unwrap(),
            RenderLength::Seconds(30.0)
        );
        for (text, seconds) in [
            ("1e-3", 0.001),
            ("1E+2", 100.0),
            ("+2", 2.0),
            ("1e-3s", 0.001),
            ("1e-3 minutes", 0.06),
            ("1e12", 1e12),
        ] {
            assert_eq!(
                parse_render_length(text).unwrap(),
                RenderLength::Seconds(seconds),
                "{text}"
            );
        }
        assert_eq!(
            parse_render_length("1e-3b").unwrap(),
            RenderLength::Cycles(0.001)
        );
        for bad in [
            "", "abc", "-3", "1:x", "5 weeks", "1e", "NaN", "inf", "1e309",
        ] {
            let error = parse_render_length(bad).unwrap_err().to_string();
            assert!(error.contains("seconds (60, 30s)"), "{bad}: {error}");
        }
    }

    /// A length in seconds of one cycle or more ends on the nearest whole
    /// cycle, so a bounce does not stop mid-bar. A shorter length and a count
    /// of cycles stay as given.
    #[test]
    fn render_lengths_end_on_a_cycle() {
        assert_eq!(RenderLength::Seconds(60.0).on_a_cycle(0.5), (60.0, true));
        assert_eq!(
            RenderLength::Seconds(60.9).on_a_cycle(0.5),
            (60.0, true),
            "nearest"
        );
        assert_eq!(RenderLength::Seconds(63.0).on_a_cycle(0.5), (64.0, true));
        assert_eq!(
            RenderLength::Seconds(0.2).on_a_cycle(0.5),
            (0.2, false),
            "under a cycle is deliberate"
        );
        assert_eq!(RenderLength::Cycles(32.0).on_a_cycle(0.5), (64.0, true));
        assert_eq!(
            RenderLength::Seconds(7.0).on_a_cycle(0.0),
            (7.0, false),
            "no tempo, as asked"
        );
        assert_eq!(format_cycles(32.0), "32");
        assert_eq!(format_cycles(2.5), "2.50");
    }

    /// The output's extension says the format, and a missing output lands
    /// beside the score with the format's extension.
    #[test]
    fn render_output_names_carry_their_format() {
        use std::path::Path;
        assert!(matches!(
            format_for_output(Path::new("take.MP3")),
            RenderCliFormat::Mp3
        ));
        assert!(matches!(
            format_for_output(Path::new("dump.json")),
            RenderCliFormat::OnsetJson
        ));
        assert!(matches!(
            format_for_output(Path::new("take.wav")),
            RenderCliFormat::ScalarWav
        ));
        assert!(matches!(
            format_for_output(Path::new("take")),
            RenderCliFormat::ScalarWav
        ));
        assert_eq!(
            default_render_output(Some(Path::new("sets/song.strudel")), RenderCliFormat::Mp3),
            PathBuf::from("sets/song.mp3")
        );
        assert_eq!(
            default_render_output(Some(Path::new("song.strudel")), RenderCliFormat::ScalarWav),
            PathBuf::from("song.wav")
        );
        assert_eq!(
            default_render_output(None, RenderCliFormat::OnsetJson),
            PathBuf::from("render.json")
        );
    }

    /// `export FILE` needs nothing else; the length forms and the flags
    /// parse; `--json` and the JSON verbosity are what make an error a JSON
    /// envelope.
    #[test]
    fn export_parses_with_only_a_score_and_json_is_asked_for_explicitly() {
        let cli = Cli::try_parse_from([product::COMMAND_NAME, "export", "song.strudel"])
            .expect("a bare export");
        match cli.command {
            Command::Render {
                output,
                duration,
                cycles,
                until_silence,
                json,
                ..
            } => {
                assert!(output.is_none() && duration.is_none() && cycles.is_none());
                assert!(!until_silence && !json);
            }
            other => panic!("not an export: {other:?}"),
        }
        let cli = Cli::try_parse_from([
            product::COMMAND_NAME,
            "render",
            "song.strudel",
            "--duration",
            "1:30",
            "--until-silence",
            "--silence-floor",
            "-50",
            "-o",
            "take.mp3",
            "--json",
        ])
        .expect("the long form");
        match cli.command {
            Command::Render {
                output,
                duration,
                until_silence,
                silence_floor,
                json,
                format,
                ..
            } => {
                assert_eq!(output, Some(PathBuf::from("take.mp3")));
                assert_eq!(duration.as_deref(), Some("1:30"));
                assert!(until_silence && json && format.is_none());
                assert_eq!(silence_floor, -50.0);
            }
            other => panic!("not a render: {other:?}"),
        }
        assert!(
            Cli::try_parse_from([
                product::COMMAND_NAME,
                "export",
                "song.strudel",
                "--duration",
                "4",
                "--cycles",
                "4"
            ])
            .is_err(),
            "a length is one thing or the other"
        );
        let wants = |line: &[&str]| {
            let mut args = vec![product::COMMAND_NAME];
            args.extend_from_slice(line);
            let cli = Cli::try_parse_from(args).expect("parses");
            cli.wants_json()
        };
        assert!(!wants(&["export", "song.strudel"]));
        assert!(wants(&["export", "song.strudel", "--json"]));
        assert!(
            wants(&["export", "song.strudel", "-vj"]),
            "a cluster is still asked"
        );
        assert!(wants(&["devices", "--json"]));
        assert!(wants(&["validate", "-j", "song.strudel"]));
        assert!(wants(&["play", "song.strudel", "-vvv"]));
        assert!(!wants(&["play", "song.strudel", "-vv"]));
        // `play` is the line a musician actually types. It had no --json of
        // its own while every other command grew one, so the only way to ask
        // was -vvv, which also turns on every live diagnostic.
        assert!(!wants(&["play", "song.strudel"]));
        assert!(
            wants(&["play", "song.strudel", "--json"]),
            "play asks like every other command"
        );
        assert!(wants(&["play", "song.strudel", "-j"]));
        assert!(
            Cli::try_parse_from([
                product::COMMAND_NAME,
                "play",
                "song.strudel",
                "--json",
                "--follow",
            ])
            .is_err(),
            "--follow draws prose to the stdout --json promises for JSON alone"
        );
        // Every command reads as prose until it is asked otherwise; none of
        // them is JSON by being itself.
        for command in [
            vec!["query", "-e", "pure(1)"],
            vec!["trace", "-e", "pure(1)"],
            vec!["bench", "-e", "pure(1)"],
        ] {
            assert!(!wants(&command), "{command:?} asked for JSON unprompted");
            let mut asked = command.clone();
            asked.push("--json");
            assert!(wants(&asked), "{asked:?} asked and was not heard");
        }
    }
}

#[cfg(feature = "studio")]
fn watch_for_studio_interrupt(
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> InterruptWatcher {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    let handle = std::thread::spawn(move || {
        while !flag.load(std::sync::atomic::Ordering::SeqCst) {
            if interrupted_by().is_some() {
                cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
    InterruptWatcher {
        done,
        handle: Some(handle),
    }
}
