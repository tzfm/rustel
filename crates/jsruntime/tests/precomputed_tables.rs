//! The scores that the query benchmarks time stay pure and walk directly
//! without building answer tables.

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
fn every_benchmarked_score_walks_pure_without_tables() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    for source in BENCHMARKED {
        runtime
            .evaluate_score(source, &TranspileOptions::default())
            .expect("evaluate");
        let pattern = runtime.active_pattern().expect("an active pattern");
        assert!(pattern.is_cacheable(), "{source}");
        let haps = pattern.query_arc(Fraction::ZERO, Fraction::int(16));
        assert!(!haps.is_empty(), "{source}");
        assert_eq!(rustel_core::table_bytes(), 0, "{source}");
    }
}
