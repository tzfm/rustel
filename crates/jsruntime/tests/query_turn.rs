//! Bounded synchronous JavaScript query turns.
//!
//! These are resource-boundary tests, not realtime claims. One deadline spans
//! an outer direct query and every reentrant callback it reaches; runnable
//! microtasks are refused and discarded rather than executed.

use rustel_core::{Pattern, QueryLimit, Value, fastcat, pure};
use rustel_fraction::Fraction;
use rustel_jsruntime::{DEFAULT_QUERY_JS_BUDGET, JsRuntime, QueryError, Slot};
use rustel_transpiler::TranspileOptions;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const QUERY_DEADLINE_CHILD: &str = "RUSTEL_QUERY_DEADLINE_CHILD";
const LEGACY_QUERY_DEADLINE_CHILD: &str = "RUSTEL_LEGACY_QUERY_DEADLINE_CHILD";

fn atom(value: &str) -> Pattern {
    pure(Value::Str(value.into()))
}

fn sequence() -> Pattern {
    fastcat(vec![atom("bd"), atom("sd")])
}

fn assert_clean(runtime: &JsRuntime, label: &str) {
    assert_eq!(runtime.query_depth(), 0, "{label}: query stack leaked");
    assert_eq!(
        rustel_jsruntime::bridge_frame_depth(),
        0,
        "{label}: bridge frame leaked"
    );
    assert_eq!(
        rustel_jsruntime::bridge_scratch_len(),
        0,
        "{label}: bridge scratch leaked"
    );
    assert!(!runtime.jobs_pending(), "{label}: pending job leaked");
}

fn semantic_runtime(source: &str) -> JsRuntime {
    let runtime = JsRuntime::new().expect("the runtime builds");
    runtime
        .install_semantic_bindings()
        .expect("the bindings install");
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("could not install `{source}`: {error}"));
    runtime
}

fn query_active(
    runtime: &JsRuntime,
    begin: Fraction,
    end: Fraction,
) -> Result<Vec<rustel_core::Hap>, QueryError> {
    runtime.query_cancellable(
        Slot::Active,
        0,
        begin,
        end,
        Duration::from_secs(5),
        &AtomicBool::new(false),
    )
}

mod query_turn {
    use super::*;

    mod and;
    mod apply;
    mod comparisons;
    mod deadlines;
    mod expand_contract;
    mod extend_replicate;
    mod keep;
    mod keepif;
    mod never_always;
    mod or;
    mod polymeter;
    mod queries;
    mod range;
    mod set;
    mod shrink_grow;
    mod shrinklist;
    mod signals;
    mod stepalt;
    mod swing;
    mod take_drop;
    mod tour;
    mod when;
    mod zip;
}
