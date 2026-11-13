//! The engine benchmark: every seam of every score, in time and memory.
//!
//! ```text
//! rustel-bench run [--quick] [--out report.json] [score or folder]...
//! rustel-bench compare base.json new.json
//! ```
//!
//! `run` evaluates and queries each score through a fresh session and
//! records, per seam, the wall time, the process CPU time, the
//! allocations and the bytes a seam leaves held. `compare` reads two
//! reports and exits with status 1 when a seam got worse past its
//! tolerance. Allocation counts and held bytes do not depend on machine
//! load, so they gate exactly. Time gates loosely: compare two runs made on
//! one machine in one session.
#![allow(clippy::print_stdout, reason = "the benchmark prints its report")]

mod measure;
mod report;
mod seams;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[global_allocator]
static ALLOCATOR: measure::Counting = measure::Counting;

/// The scores a run measures when it names none: the tracked songs.
const DEFAULT_SCORES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../runtime/examples/songs");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("compare") if args.len() == 3 => compare(Path::new(&args[1]), Path::new(&args[2])),
        _ => {
            println!("usage: rustel-bench run [--quick] [--out FILE] [SCORE|FOLDER]...");
            println!("       rustel-bench compare BASE.json NEW.json");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> ExitCode {
    let mut quick = false;
    let mut out = None;
    let mut targets = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quick" => quick = true,
            "--out" => out = args.next().map(PathBuf::from),
            target => targets.push(PathBuf::from(target)),
        }
    }
    if targets.is_empty() {
        targets.push(PathBuf::from(DEFAULT_SCORES));
    }
    let scores = scores_in(&targets);
    let plan = if quick {
        seams::Plan::QUICK
    } else {
        seams::Plan::FULL
    };
    let mut report = report::Report::default();
    for score in &scores {
        let name = score.display().to_string();
        match std::fs::read_to_string(score) {
            Ok(source) => report.push(name, seams::measure_score(&source, plan)),
            Err(error) => report.push(name, Err(error.to_string())),
        }
    }
    println!("{}", report.table());
    if let Some(path) = out
        && let Err(error) = std::fs::write(&path, report.to_json())
    {
        println!("could not write {}: {error}", path.display());
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn compare(base: &Path, new: &Path) -> ExitCode {
    let read = |path: &Path| {
        std::fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|text| report::Report::from_json(&text))
    };
    match (read(base), read(new)) {
        (Ok(base), Ok(new)) => {
            let regressions = report::regressions(&base, &new);
            for line in &regressions {
                println!("{line}");
            }
            if regressions.is_empty() {
                println!("no regression");
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        (Err(error), _) | (_, Err(error)) => {
            println!("could not read a report: {error}");
            ExitCode::from(2)
        }
    }
}

/// Every `.strudel` file named or inside a named folder, in path order.
fn scores_in(targets: &[PathBuf]) -> Vec<PathBuf> {
    let mut scores = Vec::new();
    let mut pending: Vec<PathBuf> = targets.to_vec();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                pending.extend(entries.filter_map(|entry| entry.ok().map(|entry| entry.path())));
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "strudel")
        {
            scores.push(path);
        }
    }
    scores.sort();
    scores
}
