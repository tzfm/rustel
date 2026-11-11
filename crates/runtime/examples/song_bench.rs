//! Query-time benchmark over the example songs. Run it in release:
//!
//! ```text
//! cargo run --release -p rustel-runtime --example song_bench -- all 0 16 3000
//! cargo run --release -p rustel-runtime --example song_bench -- songs/05-sections.strudel 0 16 3000
//! ```
//!
//! The first argument is `all` or one song path. Each song is evaluated once,
//! then queried `iters` times for cycles `begin..end`, three rounds; the
//! middle round is the number. Compare runs from the same day and the same
//! machine only: day-to-day drift hides small changes. The hap total must not
//! change between two versions being compared; only the time may.

use rustel_runtime::Session;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let target = args[1].clone();
    let begin: i128 = args[2].parse().expect("begin cycle");
    let end: i128 = args[3].parse().expect("end cycle");
    let iters: usize = args[4].parse().expect("iterations");
    let songs = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/songs"))
        .and_then(|entries| {
            let mut songs: Vec<std::path::PathBuf> = entries
                .filter_map(|entry| {
                    let path = entry.ok()?.path();
                    (path.extension()?.to_str()? == "strudel").then_some(path)
                })
                .collect();
            songs.sort();
            Ok(songs)
        })
        .expect("read songs folder");
    let selected: Vec<std::path::PathBuf> = if target == "all" {
        songs
    } else {
        vec![std::path::PathBuf::from(target)]
    };
    if selected.is_empty() {
        panic!("no songs found");
    }
    for path in &selected {
        let name = path.file_stem().expect("song name");
        let mut source = std::fs::read_to_string(path).expect("read score");
        if let Some(rest) = source.strip_prefix("---") {
            if let Some(stop) = rest.find("\n---") {
                source = rest[stop + 4..].to_string();
            }
        }
        let config = rustel_runtime::SessionConfig::default()
            .with_dsp_dispatch(rustel_audio::DspDispatch::portable());
        let mut session = Session::with_config(config).expect("session");
        session.evaluate(&source).expect("evaluate");
        let mut rounds = [0f64; 3];
        let mut haps = 0usize;
        for round in rounds.iter_mut() {
            let started = Instant::now();
            for _ in 0..iters {
                haps = session
                    .query(
                        rustel_fraction::Fraction::new(begin, 1),
                        rustel_fraction::Fraction::new(end, 1),
                    )
                    .expect("query")
                    .len();
            }
            *round = started.elapsed().as_micros() as f64 / iters as f64;
        }
        let mut sorted = rounds;
        sorted.sort_by(|a, b| a.total_cmp(b));
        println!(
            "{}: haps {haps} | {} us/query (rounds {:.1} {:.1} {:.1})",
            name.to_string_lossy(),
            sorted[1],
            rounds[0],
            rounds[1],
            rounds[2]
        );
    }
}
