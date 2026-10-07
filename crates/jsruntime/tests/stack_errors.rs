//! A stack keeps healthy siblings when a child's query throws.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use rustel_core::{Hap, QueryLimit, TimeSpan, Value};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, QueryError, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
}

fn evaluate(runtime: &JsRuntime, source: &str) {
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("could not evaluate `{source}`: {error}"));
}

fn query(runtime: &JsRuntime) -> Vec<Hap> {
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("an ordinary child throw must not refuse the stack query")
}

fn shape(haps: Vec<Hap>) -> Vec<(Option<TimeSpan>, TimeSpan, Value)> {
    haps.into_iter()
        .map(|hap| (hap.whole, hap.part, hap.value))
        .collect()
}

fn assert_logged(runtime: &JsRuntime, message: &str) {
    let logs = runtime.take_logs();
    assert!(
        logs.iter().any(|line| line.contains(message)),
        "missing `{message}` diagnostic: {logs:?}"
    );
}

#[test]
fn throwing_query_and_value_callbacks_keep_siblings_at_every_position() {
    let runtime = runtime();
    evaluate(&runtime, "stack(pure('left'), pure('right').fast(2))");
    let expected = shape(query(&runtime));
    assert_eq!(expected.len(), 3);

    for (child, message) in [
        (
            "new Pattern(() => { throw new Error('stack-query-sentinel'); })",
            "stack-query-sentinel",
        ),
        (
            "pure('broken').fmap(() => { throw new Error('stack-map-sentinel'); })",
            "stack-map-sentinel",
        ),
    ] {
        for position in 0..=2 {
            let mut children = vec!["pure('left')", "pure('right').fast(2)"];
            children.insert(position, child);
            let source = format!("stack({})", children.join(", "));
            evaluate(&runtime, &source);
            assert_eq!(shape(query(&runtime)), expected, "{source}");
            assert_logged(&runtime, message);
        }
    }

    evaluate(&runtime, "pure('recovered')");
    assert_eq!(query(&runtime)[0].value.show(), "recovered");
}

#[test]
fn nested_stacks_log_each_failed_child_and_keep_healthy_children() {
    let runtime = runtime();
    evaluate(
        &runtime,
        "stack(pure('left'), pure('middle'), pure('right'))",
    );
    let expected = shape(query(&runtime));
    evaluate(
        &runtime,
        r#"stack(
            pure('left'),
            stack(
                new Pattern(() => { throw new Error('nested-query-sentinel'); }),
                pure('middle'),
                pure('broken').fmap(() => { throw new Error('nested-map-sentinel'); })
            ),
            pure('right')
        )"#,
    );
    assert_eq!(shape(query(&runtime)), expected);
    let logs = runtime.take_logs();
    for message in ["nested-query-sentinel", "nested-map-sentinel"] {
        assert!(
            logs.iter().any(|line| line.contains(message)),
            "missing `{message}` diagnostic: {logs:?}"
        );
    }
}

#[test]
fn a_throw_discards_all_partial_haps_from_the_failed_child() {
    let runtime = runtime();
    evaluate(&runtime, "stack(pure('left'), pure('right'))");
    let expected = shape(query(&runtime));
    evaluate(
        &runtime,
        r#"stack(
            pure('left'),
            fastcat(pure('partial'), pure('broken')).fmap(value => {
                if (value === 'broken') throw new Error('partial-child-sentinel');
                return value;
            }),
            pure('right')
        )"#,
    );
    assert_eq!(shape(query(&runtime)), expected);
    assert_logged(&runtime, "partial-child-sentinel");
}

#[test]
fn native_semantic_child_errors_are_isolated_and_logged() {
    let runtime = runtime();
    evaluate(&runtime, "stack(pure('left'), pure('right'))");
    let expected = shape(query(&runtime));
    evaluate(
        &runtime,
        "stack(pure('left'), pure(1).add('invalid-numeral'), pure('right'))",
    );
    assert_eq!(shape(query(&runtime)), expected);
    assert_logged(
        &runtime,
        "cannot parse as numeral: \"1\" or \"invalid-numeral\"",
    );
}

#[test]
fn direct_query_arc_keeps_healthy_siblings_and_logs_child_errors() {
    let runtime = runtime();
    for (child, message) in [
        (
            "new Pattern(() => { throw new Error('direct-query-sentinel'); })",
            "direct-query-sentinel",
        ),
        (
            "pure('broken').fmap(() => { throw new Error('direct-map-sentinel'); })",
            "direct-map-sentinel",
        ),
        (
            "pure(1).add('direct-invalid-numeral')",
            "cannot parse as numeral: \"1\" or \"direct-invalid-numeral\"",
        ),
    ] {
        evaluate(
            &runtime,
            &format!(
                "const haps = stack(pure('left'), {child}, pure('right')).queryArc(0, 1);\n\
                 globalThis.stackQueryValues = JSON.stringify(haps.map(hap => hap.value));\n\
                 pure('accepted')"
            ),
        );
        assert_eq!(
            runtime.get_string("stackQueryValues").as_deref(),
            Some(r#"["left","right"]"#),
            "{child}"
        );
        assert!(
            !rustel_core::query_interrupted(),
            "direct queryArc leaked a child failure: {child}"
        );
        assert_logged(&runtime, message);
        assert_eq!(query(&runtime)[0].value.show(), "accepted");
        assert!(
            !rustel_core::query_interrupted(),
            "the following healthy query retained a child failure: {child}"
        );
    }
}

#[test]
fn a_child_pending_job_refusal_remains_fatal_and_discards_the_job() {
    let runtime = runtime();
    evaluate(
        &runtime,
        r#"stack(
            pure('left'),
            new Pattern(() => {
                queueMicrotask(() => { globalThis.stackJobRan = 1; });
                throw new Error('throw after pending job');
            }),
            pure('right').fmap(value => {
                globalThis.stackAfterRefusal = 1;
                return value;
            })
        )"#,
    );
    let result = runtime.query_cancellable(
        Slot::Active,
        0,
        Fraction::ZERO,
        Fraction::ONE,
        Duration::from_secs(1),
        &AtomicBool::new(false),
    );
    assert!(
        matches!(result, Err(QueryError::Limit(QueryLimit::JsPendingJobs))),
        "the stack must not contain a typed refusal: {result:?}"
    );
    assert_eq!(runtime.get_number("stackJobRan"), None);
    assert_eq!(runtime.get_number("stackAfterRefusal"), None);
    assert_eq!(runtime.query_depth(), 0);
    assert!(!runtime.jobs_pending());

    evaluate(&runtime, "pure('recovered')");
    assert_eq!(query(&runtime)[0].value.show(), "recovered");
    assert_eq!(runtime.get_number("stackJobRan"), None);
}
