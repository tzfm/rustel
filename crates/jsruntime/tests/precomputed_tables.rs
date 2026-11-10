//! The scores that the query benchmarks time answer from precomputed tables.
//!
//! A table is the fast path of a query. A change that leaves one of these
//! scores to the graph walk still answers right, and shows only as a slower
//! `rustel bench`. This test holds the path itself, for the order the
//! scheduler asks in and the sorted order of a host query.

use rustel_core::TimeSpan;
use rustel_fraction::Fraction;
use rustel_jsruntime::JsRuntime;
use rustel_transpiler::TranspileOptions;

const BENCHMARKED: [&str; 7] = [
    r#"n("0 .. 63")"#,
    r#"n("0 .. 63").iter(64).fast(64)"#,
    r#"stack(s("bd*4"), s("hh*8"), s("~ cp"), note("c e g").slow(2), n("0 2 4 7"), s("rim*3"), s("oh*2"), s("mt lt ht"))"#,
    "rand.segment(128)",
    r#"s("bd(3,8) sd(5,8,2) hh*8").gain("0.8 0.6")"#,
    r#"note("c e g b").fast(2).every(3, x => x.rev()).add(note("<0 12>")).lpf(800)"#,
    r#"n("0 .. 7").scale("c:major").s("piano")"#,
];

#[test]
fn every_benchmarked_score_answers_from_tables() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    let span = TimeSpan::new(Fraction::ZERO, Fraction::int(16));
    for source in BENCHMARKED {
        runtime
            .evaluate_score(source, &TranspileOptions::default())
            .expect("evaluate");
        let pattern = runtime.active_pattern().expect("an active pattern");
        assert!(pattern.answers_from_tables(span), "{source}");
    }
}
