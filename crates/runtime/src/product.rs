//! Product identity.
//!
//! Product-owned names are generated from one declaration so a pre-release
//! rename does not require hunting through runtime modules or CLI help text.

macro_rules! define_product {
    (
        display: $display_name:literal,
        command: $command_name:literal,
        env_prefix: $env_prefix:literal,
        data_dir: $data_dir:literal,
        cache_dir: $cache_dir:literal,
        session_suffix: $session_suffix:literal
    ) => {
        /// Human-readable product name used in diagnostics and UI chrome.
        pub const NAME: &str = $display_name;
        /// Installed command shown in generated help and suggestions.
        pub const COMMAND_NAME: &str = $command_name;

        pub const SESSION_DIRECTORY_ENV: &str = concat!($env_prefix, "_SESSION_DIR");
        pub const SAMPLE_CACHE_ENV: &str = concat!($env_prefix, "_SAMPLE_CACHE");
        pub const LOCAL_SAMPLES_ENV: &str = concat!($env_prefix, "_LOCAL_SAMPLES");
        pub const DATA_DIRECTORY_NAME: &str = $data_dir;
        pub const CACHE_DIRECTORY_NAME: &str = $cache_dir;
        pub const SESSION_FILE_SUFFIX: &str = $session_suffix;

        // Clap requires static help strings. Synthesize every example that
        // names the executable from the same command literal.
        pub const CLI_LONG_ABOUT: &str = concat!(
            "Run `",
            $command_name,
            " song.strudel` until Ctrl-C, add `--watch` to reload stable saves, or add ",
            "`--export out.wav` for a deterministic scalar bounce. The play / render / ",
            "query / bench / devices / check / validate / doc subcommands remain available ",
            "for automation.\n\nMost commands print text for a person; `--json` prints only ",
            "JSON for a script. `clear-score-cache` always reports JSON. Tab completion: `",
            $command_name,
            " completions zsh > ~/.zfunc/_",
            $command_name,
            "` (bash, fish, elvish and powershell too) and source it from your shell's rc.",
            "\n\nCommands and flags are identical on Linux, macOS, and ",
            "Windows; only shell quoting and file paths differ between them."
        );
        pub const RENDER_LONG_ABOUT: &str = concat!(
            "Bounce a score to a file, offline: no audio device is opened, and the same ",
            "score gives the same bytes every run.\n\n    ",
            $command_name,
            " export song.strudel                  # song.wav beside the score, 8 cycles\n    ",
            $command_name,
            " export song.strudel -o take.mp3      # mp3 from the extension, 320 kbps\n    ",
            $command_name,
            " export song.strudel --duration 1:30  # a minute and a half, ending on a cycle\n    ",
            $command_name,
            " export song.strudel --cycles 32      # a musical length\n    ",
            $command_name,
            " export song.strudel --until-silence  # and then wait for the tail to fade\n\n",
            "The file is 16-bit stereo WAV at 48 kHz unless --format or --sample-rate say ",
            "otherwise; an output ending in .mp3 is an mp3, in .json the onset dump. A ",
            "--duration in seconds (60, 30s), minutes (2m, 1:30) or bars (16b) ends on the ",
            "nearest whole cycle, so a bounce never stops mid-bar. --until-silence keeps ",
            "rendering after the length until the music has stayed under --silence-floor for ",
            "--silence-hold, so reverbs and delays ring out; a score that never goes quiet ",
            "stops a minute past its length. `render` is the same command."
        );
        pub const FOLLOW_LONG_HELP: &str = concat!(
            "Print the active code to the terminal as it changes.\n\nSugar for `| ",
            $command_name,
            " watch-code`, which is the same renderer and works from another terminal or ",
            "over ssh; this is for when you just want it on screen without a pipeline."
        );
        pub const SERVE_SAMPLES_LONG_ABOUT: &str = concat!(
            "Serve a folder of samples to a browser, so strudel.cc (or any web build) can ",
            "play them.\n\nLinux/macOS:\n    cd ~/my-samples && ",
            $command_name,
            " serve-samples\n\nWindows (PowerShell):\n    cd $HOME\\my-samples; ",
            $command_name,
            " serve-samples\n\nThe CLI itself needs none of this - `samples('local:')` reads the ",
            "folder directly. A browser cannot read a folder at all, which is the only ",
            "reason a server exists."
        );
        pub const WATCH_CODE_LONG_ABOUT: &str = concat!(
            "Show the code a set is playing, read from a piped event stream.\n\nLinux/macOS:\n    ",
            $command_name,
            " song.strudel --watch --score-events 2>&1 >/dev/null | ",
            $command_name,
            " watch-code\n    ",
            $command_name,
            " replay take",
            $session_suffix,
            " --score-events 2>&1 >/dev/null | ",
            $command_name,
            " watch-code\n\nWindows (PowerShell):\n    ",
            $command_name,
            " song.strudel --watch --score-events 2>&1 | ",
            $command_name,
            " watch-code\n\nRenders `score_active` events, so it follows a live set and a replay ",
            "alike, from another terminal or over ssh."
        );
        pub const MIDI_NO_OUTPUTS_HINT: &str = concat!(
            "no MIDI output ports. macOS/Linux: ",
            $command_name,
            " can create its own virtual port. Windows: install a loopback driver such as ",
            "loopMIDI, then pass its name to .midi()"
        );
        pub const MIDI_NO_INPUTS_HINT: &str = concat!(
            "no MIDI input ports. Plug a controller in, then run ",
            $command_name,
            " midi-monitor to watch what it sends"
        );
        pub const SESSION_FILE_HELP: &str = concat!(
            "Where to write the recording (default: `~/",
            $data_dir,
            "/sessions/<song>-<timestamp>",
            $session_suffix,
            "`)."
        );
    };
}

define_product!(
    display: "rustel",
    command: "rustel",
    env_prefix: "RUSTEL",
    data_dir: ".rustel",
    cache_dir: "rustel",
    session_suffix: ".rustel-session"
);

pub const SESSIONS_DIRECTORY_NAME: &str = "sessions";
/// Finished audio takes recorded from Studio. Session tapes remain in
/// `sessions`; this directory is imported into the sample library.
pub const RECORDINGS_DIRECTORY_NAME: &str = "recordings";
pub const SAMPLES_DIRECTORY_NAME: &str = "samples";
/// Bounces the studio writes into a set. The sample scan reads it too, to
/// leave a set's bounces alone.
pub const EXPORT_DIRECTORY_NAME: &str = "exports";
/// The folders the studio writes into a set: takes and tapes, and bounces.
/// Neither is an instrument, so neither is read for samples.
pub const SET_OUTPUT_DIRECTORY_NAMES: [&str; 2] = [SESSIONS_DIRECTORY_NAME, EXPORT_DIRECTORY_NAME];
/// The score file extension. Deliberately NOT part of `define_product!`:
/// `.strudel` names the language the score is written in, shared with
/// strudel.cc, so a product rename must not rename it.
pub const SCORE_FILE_SUFFIX: &str = ".strudel";

/// Semantic version, from this crate's manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Target triple supplied by Cargo to this crate's build script.
pub const BUILD_TARGET: &str = env!("RUSTEL_BUILD_TARGET");

/// Cargo profile family used for this binary.
pub const BUILD_PROFILE: &str = env!("RUSTEL_BUILD_PROFILE");

/// The revision a release was built from, when the build system supplies one.
///
/// `None` in an ordinary build, and that is the point: a source archive has no
/// `.git`, and running `git` from a build script makes such a build fail or -
/// worse - quietly claim the wrong revision. Release automation sets
/// `BUILD_REVISION`; nothing else needs to.
pub const REVISION: Option<&str> = option_env!("BUILD_REVISION");

/// Identity stored with a recorded session for diagnostics on another machine.
pub fn engine_identity() -> serde_json::Value {
    let mut engine = serde_json::json!({ "name": NAME, "version": VERSION });
    if let Some(revision) = REVISION {
        engine["revision"] = serde_json::Value::from(revision);
    }
    engine
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_diagnostics_follow_the_product_identity() {
        let identity = engine_identity();
        assert_eq!(identity["name"], NAME);
        assert_eq!(identity["version"], VERSION);
        assert!(!BUILD_TARGET.is_empty());
        assert!(!BUILD_PROFILE.is_empty());
    }

    #[test]
    fn generated_product_copy_uses_the_declared_command() {
        for copy in [
            CLI_LONG_ABOUT,
            FOLLOW_LONG_HELP,
            SERVE_SAMPLES_LONG_ABOUT,
            WATCH_CODE_LONG_ABOUT,
            MIDI_NO_OUTPUTS_HINT,
            MIDI_NO_INPUTS_HINT,
        ] {
            assert!(copy.contains(COMMAND_NAME), "{copy:?}");
        }
    }
}
