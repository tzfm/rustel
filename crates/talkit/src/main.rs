//! The talkit command line: speak text with key=value parameters.

use std::io::Write;
use std::process::ExitCode;

use rustel_talkit::params::Params;
use rustel_talkit::personality::Personality;
use rustel_talkit::{RATE, playback, render, wav};

const USAGE: &str = "\
talkit - speak text through the default output device

usage: talkit [key=value ...] [words ...]

Bare words are the text to speak, in the order given. A `text=` pair joins
them the same way, for text that itself contains an `=` sign.

keys:
  personality=NAME  a Talk It! voice preset; also `voice` or `v`
  pitch=50..500     the pitch the voice starts on, in Hz; default 100; also `p`
  speed=50..400     speaking speed; 150 is nominal; also `rate` or `s`
  quality=natural|monotone|sung; also `q`
  effort=normal|breathy|whispered; also `e`
  lang=english|spanish; also `language` or `l`
  out=FILE.wav      write a WAV file instead of playing; also `o`

A personality applies its whole preset, so put overrides after it.

examples:
  talkit hello world
  talkit personality=little-robot quality=monotone \"I am a robot\"
  talkit lang=spanish out=hola.wav \"hola mundo\"
";

/// The usage text with the voice names, as [`Personality::parse`] reads them.
fn help() -> String {
    let voices: Vec<&str> = Personality::ALL.iter().map(|voice| voice.name()).collect();
    format!("{USAGE}\nvoices:\n  {}\n", voices.join(" "))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", help());
        return ExitCode::SUCCESS;
    }
    if args.is_empty() {
        eprint!("{}", help());
        return ExitCode::from(2);
    }

    let mut params = Params::default();
    let mut words: Vec<String> = Vec::new();
    let mut out: Option<String> = None;

    for arg in &args {
        let Some((key, value)) = arg.split_once('=') else {
            words.push(arg.clone());
            continue;
        };
        match key.to_ascii_lowercase().as_str() {
            "out" | "o" => out = Some(value.to_string()),
            "text" => words.push(value.to_string()),
            _ => {
                if let Err(e) = params.apply(key, value) {
                    eprintln!("talkit: {e}");
                    return ExitCode::from(2);
                }
            }
        }
    }

    let text = words.join(" ");
    if text.trim().is_empty() {
        eprintln!("talkit: nothing to say");
        return ExitCode::from(2);
    }

    let samples = render::render(&text, &params);
    let result = match &out {
        Some(path) => wav::write_wav(path, &samples, RATE).map(|_| ()),
        None => playback::play(&samples, RATE),
    };
    if let Err(e) = result {
        let _ = std::io::stderr().flush();
        eprintln!("talkit: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
