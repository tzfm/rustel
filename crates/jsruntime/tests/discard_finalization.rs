//! Discarding a job can enqueue FinalizationRegistry cleanup jobs. None may
//! escape a refused turn and run during a later setup evaluation.

use rustel_core::QueryLimit;
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, QueryError, Slot};
use rustel_transpiler::TranspileOptions;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(5);

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score("s('bd')", &TranspileOptions::default())
        .unwrap();
    runtime
        .eval(
            r#"
            globalThis.discardedJobRan = 0;
            globalThis.cleanupRan = 0;
            globalThis.registry = new FinalizationRegistry(() => { cleanupRan++; });
            globalThis.queueFinalizationChain = () => {
                let target = {};
                queueMicrotask(((held) => () => {
                    discardedJobRan++;
                    return held;
                })(target));
                // Each held value becomes the next target. Discarding the
                // original job therefore produces five successive cleanup
                // jobs, which cannot all belong to the original snapshot.
                for (let i = 0; i < 4; i++) {
                    const next = {};
                    registry.register(target, next);
                    target = next;
                }
                registry.register(target, 0);
            };
            "#,
        )
        .unwrap();
    runtime
}

fn assert_clean_and_reusable(runtime: &JsRuntime) {
    assert!(
        !runtime.jobs_pending(),
        "finalizer-created job escaped cleanup"
    );
    runtime
        .evaluate_prelude(
            "globalThis.recovered = 1;",
            &TranspileOptions::default(),
            BUDGET,
        )
        .expect("the next setup must succeed without a stale-job refusal");
    assert_eq!(runtime.get_number("recovered"), Some(1.0));
    assert_eq!(runtime.get_number("discardedJobRan"), Some(0.0));
    assert_eq!(runtime.get_number("cleanupRan"), Some(0.0));
    assert!(!runtime.jobs_pending());
    assert!(
        !runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn stale_job_cleanup_drains_finalization_jobs_even_when_cancelled() {
    for cancelled in [false, true] {
        let runtime = runtime();
        runtime.eval("queueFinalizationChain();").unwrap();
        assert!(runtime.jobs_pending());
        let error = runtime
            .query_cancellable(
                Slot::Active,
                0,
                Fraction::ZERO,
                Fraction::ONE,
                BUDGET,
                &AtomicBool::new(cancelled),
            )
            .unwrap_err();
        if cancelled {
            assert!(matches!(error, QueryError::Limit(QueryLimit::Cancelled)));
        } else {
            assert!(matches!(
                error,
                QueryError::Limit(QueryLimit::JsPendingJobs)
            ));
        }
        assert_clean_and_reusable(&runtime);
    }
}

#[test]
fn refused_query_drains_finalization_jobs() {
    let runtime = runtime();
    runtime
        .evaluate_score(
            "s('bd').fmap(value => { if (!globalThis.queuedChain) { globalThis.queuedChain = true; queueFinalizationChain(); } return value; })",
            &TranspileOptions::default(),
        )
        .unwrap();
    let error = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::JsPendingJobs)
    ));
    assert_clean_and_reusable(&runtime);
}

#[test]
fn refused_score_drains_finalization_jobs() {
    let runtime = runtime();
    let error = runtime
        .evaluate_score_cancellable(
            "queueFinalizationChain(); s('sd')",
            &TranspileOptions::default(),
            BUDGET,
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::JsPendingJobs)
    ));
    assert_clean_and_reusable(&runtime);
}

#[test]
fn failed_prelude_drains_finalization_jobs() {
    let runtime = runtime();
    let error = runtime
        .evaluate_prelude(
            "queueMicrotask(() => { throw new Error('stop before chain'); }); queueFinalizationChain();",
            &TranspileOptions::default(),
            BUDGET,
        )
        .unwrap_err();
    assert!(error.to_string().contains("stop before chain"), "{error}");
    assert_clean_and_reusable(&runtime);
}
