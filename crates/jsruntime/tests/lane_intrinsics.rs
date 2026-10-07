//! REPL lane bookkeeping remains host-owned after rejected score mutations.

use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
}

#[test]
fn rejected_intrinsic_mutation_does_not_poison_the_next_lane_save() {
    let runtime = runtime();
    let rejected = runtime
        .evaluate_score(
            r#"
              Object.entries = () => { throw new Error('poisoned Object.entries'); };
              String.prototype.startsWith = () => { throw new Error('poisoned startsWith'); };
              String.prototype.endsWith = () => { throw new Error('poisoned endsWith'); };
              String.prototype.includes = () => { throw new Error('poisoned includes'); };
              throw new Error('reject after intrinsic mutation');
              pure('unreachable')
            "#,
            &TranspileOptions::default(),
        )
        .expect_err("the first save deliberately rejects after mutating intrinsics");
    assert!(
        rejected.contains("reject after intrinsic mutation"),
        "wrong first-save error: {rejected}"
    );

    runtime
        .evaluate_score(
            "pure('recovered').p('Srecovered')",
            &TranspileOptions::default(),
        )
        .expect("private lane finalization must recover on the next save");
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query recovered score");
    assert_eq!(haps.len(), 1);
    assert!(haps[0].show().contains("recovered"), "{}", haps[0].show());
}

/// Pins the REPL's `hush()`: it returns silence that still takes pattern
/// methods, and drops the lanes and `all` transform collected before it while
/// lanes collected after it play.
#[test]
fn hush_clears_the_lanes_before_it_and_keeps_the_ones_after() {
    let runtime = runtime();
    let query = |source: &str| {
        runtime
            .evaluate_score(source, &TranspileOptions::default())
            .expect("evaluate");
        runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query")
    };

    let haps = query("hush().color('white')");
    assert!(haps.is_empty(), "hush() is not silent: {haps:?}");

    let haps = query(
        r#"
          all(x => x.fast(2));
          pure('gone').p('gone');
          hush();
          pure('after').p('after');
        "#,
    );
    assert_eq!(haps.len(), 1, "{haps:?}");
    assert!(haps[0].show().contains("after"), "{}", haps[0].show());
}
