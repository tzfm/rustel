//! The scheduler applies the `queryArc` error boundary through
//! `Pattern::query_state`, while preserving the `_cps` control.
//! Invalid operations produce no haps, matching strudel.cc. Calling the raw
//! `Pattern::query` boundary can instead pass invalid values to onset events.

use rustel_core::{
    CallbackHost, Hap, Pattern, QueryLimit, State, Value, fastcat, js_query, pure,
    refuse_host_memory, stack, with_callback_host,
};
use rustel_scheduler::{Clock, Scheduler, TickStatus, Transport, VirtualClock};
use std::sync::Arc;

const CPS: f64 = 1.0;
const HORIZON: f64 = 0.5;

/// `"a b".add("x")` - both operands are non-numeric strings, so `numeralArgs`
/// runs `parseNumeral`, which throws.
fn throws_mid_query() -> Pattern {
    let registry = rustel_core::register::default_registry();
    let _ = &registry;
    let words = fastcat(vec![
        pure(Value::Str("a".into())),
        pure(Value::Str("b".into())),
    ]);
    rustel_core::compose::compose(
        &words,
        &pure(Value::Str("x".into())),
        rustel_core::compose::ComposeOp::Add,
        rustel_core::compose::Alignment::In,
    )
}

fn onsets_over(pattern: Pattern, seconds: f64) -> usize {
    let transport = Arc::new(Transport::default());
    let mut scheduler = Scheduler::new(transport.clone(), CPS, HORIZON);
    let clock = VirtualClock::new(0.0);
    transport.start();
    scheduler.set_pattern(pattern, clock.now());

    let mut count = 0;
    let mut elapsed = 0.0;
    while elapsed <= seconds {
        let _ = scheduler.tick(&clock);
        count += scheduler.drain_due(&clock).len();
        clock.advance(0.05);
        elapsed += 0.05;
    }
    count
}

#[test]
fn a_pattern_that_throws_in_strudel_schedules_nothing() {
    assert_eq!(
        onsets_over(throws_mid_query(), 2.0),
        0,
        "a pattern whose query throws strudel.cc is SILENT there; scheduling \
         onsets for it means emitting audio strudel.cc would not"
    );
}

#[test]
fn a_throwing_stack_child_does_not_silence_other_voices() {
    let healthy = pure(Value::Str("bd".into()));
    let expected = onsets_over(healthy.clone(), 2.0);
    assert!(expected > 0);
    assert_eq!(
        onsets_over(stack(vec![throws_mid_query(), healthy]), 2.0),
        expected,
        "the healthy voice must keep scheduling after its sibling throws"
    );
}

#[test]
fn a_throw_contained_by_a_stack_is_kept_apart_from_a_thrown_query() {
    let transport = Arc::new(Transport::default());
    let mut scheduler = Scheduler::new(transport.clone(), CPS, HORIZON);
    let clock = VirtualClock::new(0.0);
    transport.start();
    scheduler.set_pattern(stack(vec![throws_mid_query()]), clock.now());
    let _ = scheduler.tick(&clock);
    assert!(scheduler.drain_due(&clock).is_empty());
    assert_eq!(
        scheduler.take_thrown(),
        None,
        "the stack contained the throw"
    );
    let contained = scheduler.take_contained_throw();
    assert!(
        contained
            .as_deref()
            .is_some_and(|message| message.contains("cannot parse as numeral")),
        "{contained:?}"
    );
    assert_eq!(scheduler.take_contained_throw(), None, "taken once");
}

/// A host-memory refusal is typed through the scheduler without allocating.
///
/// The allocator-to-`HostMemory` edge is exercised in rustel-jsruntime. This
/// test isolates the scheduler boundary without relying on the exact QuickJS
/// allocation that exhausts a particular horizon.
struct HostMemoryRefuser;

impl CallbackHost for HostMemoryRefuser {
    fn call_value(&self, _id: usize, value: &Value) -> Result<Value, String> {
        Ok(value.clone())
    }

    fn call_query(&self, _id: usize, _state: &State) -> Result<Vec<Hap>, String> {
        refuse_host_memory();
        Ok(Vec::new())
    }
}

#[test]
fn a_host_memory_refusal_remains_typed_through_scheduler_tick() {
    let transport = Arc::new(Transport::default());
    let mut scheduler = Scheduler::new(transport.clone(), CPS, HORIZON);
    let clock = VirtualClock::new(0.0);
    transport.start();
    scheduler.set_pattern(js_query(7), clock.now());

    let status = with_callback_host(&HostMemoryRefuser, || scheduler.tick(&clock));
    assert_eq!(
        status,
        TickStatus::Refused,
        "a host-memory refusal must not become a successful silent tick"
    );
    assert_eq!(
        scheduler.refusal(),
        Some(&QueryLimit::HostMemory),
        "the scheduler changed or erased the refusal kind"
    );
}
