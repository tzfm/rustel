/*
rustel-core -- structural JavaScript query refusals
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use rustel_core::{
    QueryLimit, State, TimeSpan, Value, pure, refuse_js_cpu_deadline, refuse_js_pending_jobs,
    with_cancellation,
};
use rustel_fraction::Fraction;

const QUERY_BUDGET: u64 = 100;

fn one_cycle() -> State {
    State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE))
}

fn query_limit(result: Result<Vec<rustel_core::Hap>, QueryLimit>) -> QueryLimit {
    match result {
        Err(limit) => limit,
        Ok(haps) => panic!(
            "expected a typed query refusal, but the query returned {} haps",
            haps.len()
        ),
    }
}

#[test]
fn javascript_refusal_helpers_reach_the_typed_query_boundary() {
    let deadline = pure(Value::F64(1.0)).fmap(|value| {
        refuse_js_cpu_deadline(37);
        value.clone()
    });
    assert_eq!(
        query_limit(deadline.try_query_state_with_budget(&one_cycle(), QUERY_BUDGET)),
        QueryLimit::JsCpuDeadline { millis: 37 }
    );

    let pending_jobs = pure(Value::F64(1.0)).fmap(|value| {
        refuse_js_pending_jobs();
        value.clone()
    });
    assert_eq!(
        query_limit(pending_jobs.try_query_state_with_budget(&one_cycle(), QUERY_BUDGET)),
        QueryLimit::JsPendingJobs
    );
}

#[test]
fn the_first_javascript_refusal_wins_and_does_not_poison_the_next_query() {
    let deadline_first = pure(Value::F64(1.0)).fmap(|value| {
        refuse_js_cpu_deadline(41);
        refuse_js_pending_jobs();
        value.clone()
    });
    assert_eq!(
        query_limit(deadline_first.try_query_state_with_budget(&one_cycle(), QUERY_BUDGET)),
        QueryLimit::JsCpuDeadline { millis: 41 }
    );

    let jobs_first = pure(Value::F64(1.0)).fmap(|value| {
        refuse_js_pending_jobs();
        refuse_js_cpu_deadline(99);
        value.clone()
    });
    assert_eq!(
        query_limit(jobs_first.try_query_state_with_budget(&one_cycle(), QUERY_BUDGET)),
        QueryLimit::JsPendingJobs
    );

    let healthy = pure(Value::F64(1.0))
        .try_query_state_with_budget(&one_cycle(), QUERY_BUDGET)
        .expect("a completed refusal must not leak into the next query");
    assert_eq!(healthy.len(), 1);
}

#[test]
fn cancellation_takes_precedence_over_a_javascript_refusal() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&cancelled);
    let pattern = pure(Value::F64(1.0)).fmap(move |value| {
        refuse_js_cpu_deadline(43);
        callback_flag.store(true, Ordering::Relaxed);
        value.clone()
    });

    let result = with_cancellation(cancelled.as_ref(), || {
        pattern.try_query_state_with_budget(&one_cycle(), QUERY_BUDGET)
    });
    assert_eq!(query_limit(result), QueryLimit::Cancelled);
}

#[test]
fn a_nested_query_shares_the_outer_structural_refusal() {
    let inner = pure(Value::F64(2.0)).fmap(|value| {
        refuse_js_pending_jobs();
        value.clone()
    });
    let outer = pure(Value::F64(1.0)).fmap(move |value| {
        assert_eq!(
            query_limit(inner.try_query_state_with_budget(&one_cycle(), QUERY_BUDGET)),
            QueryLimit::JsPendingJobs,
            "the nested boundary must see the structural refusal"
        );
        // Ignoring the nested result must not erase its refusal from the
        // enclosing query.
        value.clone()
    });

    assert_eq!(
        query_limit(outer.try_query_state_with_budget(&one_cycle(), QUERY_BUDGET)),
        QueryLimit::JsPendingJobs
    );
}

#[test]
fn a_nested_query_cannot_replace_an_earlier_outer_refusal() {
    let inner = pure(Value::F64(2.0)).fmap(|value| {
        refuse_js_pending_jobs();
        value.clone()
    });
    let outer = pure(Value::F64(1.0)).fmap(move |value| {
        refuse_js_cpu_deadline(47);
        assert_eq!(
            query_limit(inner.try_query_state_with_budget(&one_cycle(), QUERY_BUDGET)),
            QueryLimit::JsCpuDeadline { millis: 47 },
            "a nested boundary must preserve the first outer refusal"
        );
        value.clone()
    });

    assert_eq!(
        query_limit(outer.try_query_state_with_budget(&one_cycle(), QUERY_BUDGET)),
        QueryLimit::JsCpuDeadline { millis: 47 }
    );
}
