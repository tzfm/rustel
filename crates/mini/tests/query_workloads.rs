//! Manual release benchmark for pattern queries.
//!
//! One dense pattern, two window shapes. Whole cycles from cycle 0 keep
//! every time value small. A live window starts at a cursor taken from a
//! float, as the scheduler does after an edit, so the span bounds have
//! denominators near 10^10. A third case sets five controls and
//! queries whole cycles.
//!
//! Run with:
//!
//! ```text
//! cargo test --release -p rustel-mini --test query_workloads -- --ignored --nocapture
//! ```

use std::hint::black_box;
use std::time::Instant;

use rustel_core::{Pattern, controls::ControlSpec};
use rustel_fraction::Fraction;

const REPETITIONS: usize = 7;

/// `s("[bd hh sd hh]*8").fast(2)`: 64 haps a cycle.
fn dense_pattern() -> Pattern {
    let values = rustel_mini::mini("[bd hh sd hh]*8").expect("the pattern parses");
    ControlSpec::new(["s"])
        .pattern(&values)
        .fast(Fraction::from(2_i64))
}

/// `s("[bd hh sd hh]*8")` with `n`, `gain`, `pan`, `cutoff` and `room` set: 32 haps a cycle.
fn five_control_pattern() -> Pattern {
    let controls = rustel_core::controls::default_control_registry();
    let values = rustel_mini::mini("[bd hh sd hh]*8").expect("the pattern parses");
    let mut pattern = controls
        .get("s")
        .expect("the s control exists")
        .standalone(&values);
    for (name, value) in [
        ("n", 3.0),
        ("gain", 0.8),
        ("pan", 0.25),
        ("cutoff", 800.0),
        ("room", 0.3),
    ] {
        let value = rustel_core::pure(rustel_core::Value::F64(value));
        pattern = controls
            .get(name)
            .expect("the control exists")
            .apply(&pattern, Some(value));
    }
    pattern
}

/// Query `windows` spans of `length` cycles in a row from `start`.
fn haps_per_second(pattern: &Pattern, start: Fraction, length: Fraction, windows: usize) -> f64 {
    let run = || {
        let mut begin = start;
        let mut haps = 0_usize;
        for _ in 0..windows {
            let end = begin + length;
            haps += black_box(pattern.query_arc(begin, end)).len();
            begin = end;
        }
        haps
    };
    run();
    let mut rates: Vec<f64> = (0..REPETITIONS)
        .map(|_| {
            let started = Instant::now();
            let haps = run();
            haps as f64 / started.elapsed().as_secs_f64()
        })
        .collect();
    rates.sort_by(f64::total_cmp);
    rates[REPETITIONS / 2]
}

fn require_release_build() {
    #[cfg(debug_assertions)]
    panic!("pattern query measurements must use cargo test --release");
}

#[test]
#[ignore = "manual release benchmark"]
fn dense_pattern_query_workloads() {
    require_release_build();
    let pattern = dense_pattern();
    let whole = haps_per_second(&pattern, Fraction::ZERO, Fraction::from(4_i64), 2_000);
    let cursor = Fraction::from_f64(1_000.123_456_789).expect("a finite cycle");
    let live = haps_per_second(&pattern, cursor, Fraction::new(25, 1000), 40_000);
    let controlled = haps_per_second(
        &five_control_pattern(),
        Fraction::ZERO,
        Fraction::from(4_i64),
        1_000,
    );
    // Finish libtest's status line before the records.
    println!();
    println!("{{\"case\":\"whole-cycles\",\"haps_per_sec\":{whole:.0}}}");
    println!("{{\"case\":\"live-window\",\"haps_per_sec\":{live:.0}}}");
    println!("{{\"case\":\"five-controls\",\"haps_per_sec\":{controlled:.0}}}");
}
