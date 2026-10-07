//! Positive child-process control for interrupted QuickJS jobs.
//!
//! A queued callback is interrupted after it has queued another callback. The
//! product prebake executor must retrieve the pending exception immediately,
//! discard the residual queue under the runtime lock, collect, and keep using
//! the same heap. rquickjs 0.14 fixes the old borrowed-context ownership
//! bug; this control also guards exception consumption and residual-job cleanup.

use std::time::Duration;

use rustel_core::QueryLimit;
use rustel_jsruntime::{JsRuntime, QueryError};
use rustel_transpiler::TranspileOptions;

fn main() {
    const TURNS: usize = 32;
    let rt = JsRuntime::new().expect("runtime");
    let options = TranspileOptions::default();

    for turn in 0..TURNS {
        let error = rt
            .evaluate_prelude(
                "queueMicrotask(() => { \
                   globalThis.interruptedJobStarts = \
                     (globalThis.interruptedJobStarts ?? 0) + 1; \
                   queueMicrotask(() => { globalThis.residualJobRan = 1; }); \
                   while (true) {} \
                 });",
                &options,
                Duration::from_millis(50),
            )
            .expect_err("the queued runaway must hit the shared deadline");
        assert!(
            matches!(error, QueryError::Limit(QueryLimit::JsCpuDeadline { .. })),
            "turn {turn}: wrong interrupted-job error: {error:?}"
        );
        assert_eq!(
            rt.get_number("interruptedJobStarts"),
            Some((turn + 1) as f64),
            "turn {turn}: the deadline fired before the queued callback began"
        );
        assert!(!rt.jobs_pending(), "turn {turn}: residual job survived");

        rt.run_gc();
        rt.run_gc();
        rt.evaluate_prelude(
            "await Promise.resolve(); \
             globalThis.recoveredTurns = (globalThis.recoveredTurns ?? 0) + 1;",
            &options,
            Duration::from_millis(100),
        )
        .unwrap_or_else(|error| panic!("turn {turn}: same-heap recovery failed: {error:?}"));
    }

    assert_eq!(rt.get_number("residualJobRan"), None);
    assert_eq!(rt.get_number("recoveredTurns"), Some(TURNS as f64));
    rt.run_gc();
    rt.run_gc();
    println!("JOB_INTERRUPT_RECOVERY turns={TURNS}");
}
