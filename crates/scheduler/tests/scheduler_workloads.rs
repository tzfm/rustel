//! Manual release benchmark for complete scheduler turns.
//!
//! The cases use only the public Rust pattern and scheduler boundaries. They
//! cover sparse and dense values, duplicate onsets, a partially drained
//! backlog, and the optional UI trace without depending on score syntax or a
//! particular composition.
//!
//! An untimed reference pass uses the same graph, warmup and profiled scheduler
//! path before any timed samples. Timers retain their original boundaries:
//! tick_profiled and drain_through separately; trace draining, output hashing,
//! event destruction, clock advancement and setup are outside those timers.
//!
//! Run with:
//!
//! ```text
//! cargo test --release -p rustel-scheduler --test scheduler_workloads -- --ignored --nocapture
//! ```

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use rustel_core::{Pattern, Value, fastcat, pure, stack};
use rustel_scheduler::{
    Clock, Event, ScheduleTraceEvent, Scheduler, SchedulerTickProfile, TickStatus, Transport,
    VirtualClock,
};

const CPS: f64 = 4.0;

#[derive(Clone, Copy)]
struct Workload {
    id: &'static str,
    steps: usize,
    stack_width: usize,
    control_rich: bool,
    trace: bool,
    horizon_secs: f64,
    drain_step_secs: f64,
}

const WORKLOADS: [Workload; 6] = [
    Workload {
        id: "sparse-primitives",
        steps: 8,
        stack_width: 1,
        control_rich: false,
        trace: false,
        horizon_secs: 0.25,
        drain_step_secs: 0.25,
    },
    Workload {
        id: "dense-primitives",
        steps: 64,
        stack_width: 1,
        control_rich: false,
        trace: false,
        horizon_secs: 0.25,
        drain_step_secs: 0.25,
    },
    Workload {
        id: "dense-controls",
        steps: 64,
        stack_width: 1,
        control_rich: true,
        trace: false,
        horizon_secs: 0.25,
        drain_step_secs: 0.25,
    },
    Workload {
        id: "duplicate-controls",
        steps: 32,
        stack_width: 8,
        control_rich: true,
        trace: false,
        horizon_secs: 0.25,
        drain_step_secs: 0.25,
    },
    Workload {
        id: "partial-backlog-controls",
        steps: 64,
        stack_width: 1,
        control_rich: true,
        trace: false,
        horizon_secs: 0.5,
        drain_step_secs: 0.125,
    },
    Workload {
        id: "trace-dense-controls",
        steps: 64,
        stack_width: 1,
        control_rich: true,
        trace: true,
        horizon_secs: 0.25,
        drain_step_secs: 0.25,
    },
];

fn value(step: usize, control_rich: bool) -> Value {
    if !control_rich {
        return Value::Str(format!("v{}", step % 8));
    }
    Value::object([
        ("s".into(), Value::Str(format!("sample-{}", step % 16))),
        ("n".into(), Value::F64((step % 12) as f64)),
        ("gain".into(), Value::F64(0.2 + (step % 7) as f64 * 0.1)),
        ("pan".into(), Value::F64((step % 9) as f64 / 8.0)),
        ("lpf".into(), Value::F64(400.0 + (step % 24) as f64 * 80.0)),
        ("orbit".into(), Value::F64((step % 4) as f64)),
    ])
}

fn pattern(workload: Workload) -> Pattern {
    let lane = fastcat(
        (0..workload.steps)
            .map(|step| pure(value(step, workload.control_rich)))
            .collect(),
    );
    if workload.stack_width == 1 {
        lane
    } else {
        stack(vec![lane; workload.stack_width])
    }
}

fn setting(name: &str, default: usize, maximum: usize) -> usize {
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    let value = raw
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("{name} must be a positive integer"));
    assert!(
        (1..=maximum).contains(&value),
        "{name} must be in 1..={maximum}"
    );
    value
}

fn require_release_build() {
    #[cfg(debug_assertions)]
    panic!("scheduler measurements must use cargo test --release");
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn hash_event(hash: &mut u64, event: &Event) {
    hash_bytes(hash, &event.onset_id.to_le_bytes());
    hash_bytes(hash, &event.generation.to_le_bytes());
    hash_bytes(hash, &event.whole_begin.numer().to_le_bytes());
    hash_bytes(hash, &event.whole_begin.denom().to_le_bytes());
    hash_bytes(hash, &event.whole_end.numer().to_le_bytes());
    hash_bytes(hash, &event.whole_end.denom().to_le_bytes());
    hash_bytes(hash, &event.duration.numer().to_le_bytes());
    hash_bytes(hash, &event.duration.denom().to_le_bytes());
    hash_bytes(hash, &event.target_time.to_bits().to_le_bytes());
    hash_bytes(hash, event.value.show().as_bytes());
    hash_bytes(hash, &event.ui_visuals.to_le_bytes());
    if let Some(line) = &event.log_line {
        hash_bytes(hash, line.as_bytes());
    }
}

fn hash_trace(hash: &mut u64, event: &ScheduleTraceEvent) {
    hash_bytes(hash, &event.onset_id.to_le_bytes());
    hash_bytes(hash, &event.generation.to_le_bytes());
    hash_bytes(hash, &event.whole_begin.numer().to_le_bytes());
    hash_bytes(hash, &event.whole_begin.denom().to_le_bytes());
    hash_bytes(hash, &event.target_time.to_bits().to_le_bytes());
    if let Some(value) = &event.value_show {
        hash_bytes(hash, value.as_bytes());
    }
}

fn warm_up(
    scheduler: &mut Scheduler,
    clock: &VirtualClock,
    workload: Workload,
    turns: usize,
    trace: &mut Vec<ScheduleTraceEvent>,
) {
    for _ in 0..turns {
        assert_eq!(scheduler.tick(clock), TickStatus::Filled);
        let events = scheduler.drain_through(clock, clock.now() + workload.drain_step_secs);
        assert!(
            !events.is_empty(),
            "{} produced no warm-up events",
            workload.id
        );
        black_box(&events);
        if workload.trace {
            scheduler.drain_trace_events_into(trace);
            assert!(
                !trace.is_empty(),
                "{} produced no warm-up trace",
                workload.id
            );
            black_box(&trace);
            trace.clear();
        }
        clock.advance(workload.drain_step_secs);
    }
}

#[derive(Default)]
struct Timings {
    tick_nanos: u128,
    query_nanos: u128,
    acceptance_nanos: u128,
    drain_nanos: u128,
}

#[derive(Debug, PartialEq, Eq)]
struct Validation {
    queried_haps: u64,
    accepted_events: u64,
    drained_events: u64,
    trace_events: u64,
    maximum_queue_depth: usize,
    initial_queue_depth: usize,
    final_queue_depth: usize,
    validation_hash: u64,
    counter_hash: u64,
}

// Both paths keep the scheduler's internal profiling enabled. Only the
// external service timers are compiled out of the reference pass.
fn run<const TIMED: bool>(
    workload: Workload,
    workload_pattern: &Pattern,
    warmup_turns: usize,
    measured_turns: usize,
) -> (Timings, Validation) {
    let transport = Arc::new(Transport::default());
    let mut scheduler = Scheduler::new(transport, CPS, workload.horizon_secs);
    scheduler.set_trace_enabled(workload.trace);
    let clock = VirtualClock::new(0.0);
    scheduler.set_pattern(workload_pattern.clone(), clock.now());
    let mut trace = Vec::new();
    warm_up(&mut scheduler, &clock, workload, warmup_turns, &mut trace);

    let mut timings = Timings::default();
    let mut validation = Validation {
        queried_haps: 0,
        accepted_events: 0,
        drained_events: 0,
        trace_events: 0,
        maximum_queue_depth: 0,
        initial_queue_depth: scheduler.queued(),
        final_queue_depth: 0,
        validation_hash: 0xcbf2_9ce4_8422_2325,
        counter_hash: 0xcbf2_9ce4_8422_2325,
    };
    let expected_per_turn =
        (CPS * workload.drain_step_secs * (workload.steps * workload.stack_width) as f64) as usize;
    for _ in 0..measured_turns {
        let mut profile = SchedulerTickProfile::default();
        let tick_started = TIMED.then(Instant::now);
        let status = scheduler.tick_profiled(&clock, &mut profile);
        if let Some(started) = tick_started {
            timings.tick_nanos = timings
                .tick_nanos
                .saturating_add(started.elapsed().as_nanos());
        }
        assert_eq!(status, TickStatus::Filled, "{} did not fill", workload.id);
        assert_eq!(profile.callback_calls, 0, "Rust workload entered a host");
        assert!(!profile.refused && !profile.queue_full && !profile.stopped);
        if TIMED {
            timings.query_nanos = timings
                .query_nanos
                .saturating_add(u128::from(profile.query_nanos));
            timings.acceptance_nanos = timings
                .acceptance_nanos
                .saturating_add(u128::from(profile.acceptance_nanos));
        }
        validation.queried_haps += profile.queried_haps;
        validation.accepted_events += profile.accepted_events;
        let queue_depth = scheduler.queued();
        validation.maximum_queue_depth = validation.maximum_queue_depth.max(queue_depth);

        let drain_started = TIMED.then(Instant::now);
        let events = scheduler.drain_through(&clock, clock.now() + workload.drain_step_secs);
        if let Some(started) = drain_started {
            timings.drain_nanos = timings
                .drain_nanos
                .saturating_add(started.elapsed().as_nanos());
        }
        assert_eq!(
            events.len(),
            expected_per_turn,
            "{} incomplete turn",
            workload.id
        );
        validation.drained_events += events.len() as u64;
        for event in &events {
            hash_event(&mut validation.validation_hash, event);
        }
        black_box(&events);

        if workload.trace {
            scheduler.drain_trace_events_into(&mut trace);
            for event in &trace {
                hash_trace(&mut validation.validation_hash, event);
            }
            black_box(&trace);
        }
        validation.trace_events += trace.len() as u64;
        // Preserve the original output hash and additionally pin every turn's
        // work/queue distribution, not merely equal totals across the window.
        for count in [
            profile.query_span_millicycles,
            profile.queried_haps,
            profile.accepted_events,
            events.len() as u64,
            trace.len() as u64,
            queue_depth as u64,
            scheduler.queued() as u64,
        ] {
            hash_bytes(&mut validation.counter_hash, &count.to_le_bytes());
        }
        trace.clear();
        clock.advance(workload.drain_step_secs);
    }
    validation.final_queue_depth = scheduler.queued();
    assert_eq!(validation.initial_queue_depth, validation.final_queue_depth);
    assert_eq!(validation.accepted_events, validation.drained_events);
    assert_eq!(
        validation.trace_events,
        if workload.trace {
            validation.accepted_events
        } else {
            0
        }
    );
    (timings, validation)
}

#[test]
fn scheduler_workloads_match_untimed_reference() {
    for (workload, expected_events) in WORKLOADS.into_iter().zip([32, 256, 256, 1024, 128, 256]) {
        let workload_pattern = pattern(workload);
        let (_, reference) = run::<false>(workload, &workload_pattern, 2, 4);
        let (_, measured) = run::<true>(workload, &workload_pattern, 2, 4);
        assert_eq!(measured, reference, "{} validation differs", workload.id);
        assert_eq!(
            measured.accepted_events, expected_events,
            "{} accepted",
            workload.id
        );
        assert_eq!(
            measured.drained_events, expected_events,
            "{} drained",
            workload.id
        );
        assert_eq!(
            measured.trace_events,
            if workload.trace { expected_events } else { 0 },
            "{} trace",
            workload.id
        );
    }
}

#[test]
#[ignore = "manual release benchmark"]
fn complete_scheduler_turn_workloads() {
    require_release_build();
    // Finish libtest's status line before emitting JSONL records.
    println!();
    let repetitions = setting("RUSTEL_SCHEDULER_BENCH_REPETITIONS", 7, 100);
    let warmup_turns = setting("RUSTEL_SCHEDULER_BENCH_WARMUP_TURNS", 128, 100_000);
    let measured_turns = setting("RUSTEL_SCHEDULER_BENCH_TURNS", 2_048, 1_000_000);
    let selected = std::env::var("RUSTEL_SCHEDULER_BENCH_CASE").ok();
    let mut matched = false;

    for workload in WORKLOADS {
        if selected.as_deref().is_some_and(|id| id != workload.id) {
            continue;
        }
        matched = true;
        let workload_pattern = pattern(workload);
        let (_, reference) =
            run::<false>(workload, &workload_pattern, warmup_turns, measured_turns);
        for repetition in 1..=repetitions {
            let (timings, validation) =
                run::<true>(workload, &workload_pattern, warmup_turns, measured_turns);
            assert_eq!(
                validation, reference,
                "{} differs from untimed reference",
                workload.id
            );
            let event_denominator = validation.drained_events.max(1) as f64;
            println!(
                "{{\"schema_version\":1,\"benchmark\":\"complete-scheduler-turn\",\"workload\":\"{}\",\"repetition\":{},\"reference_passes\":1,\"warmup_turns\":{},\"measured_turns\":{},\"queried_haps\":{},\"accepted_events\":{},\"drained_events\":{},\"trace_events\":{},\"maximum_queue_depth\":{},\"tick_nanos\":{},\"query_nanos\":{},\"acceptance_nanos\":{},\"drain_nanos\":{},\"tick_nanos_per_event\":{:.6},\"query_nanos_per_hap\":{:.6},\"acceptance_nanos_per_event\":{:.6},\"drain_nanos_per_event\":{:.6},\"validation_hash\":\"{:016x}\",\"counter_hash\":\"{:016x}\",\"initial_queue_depth\":{},\"final_queue_depth\":{}}}",
                workload.id,
                repetition,
                warmup_turns,
                measured_turns,
                validation.queried_haps,
                validation.accepted_events,
                validation.drained_events,
                validation.trace_events,
                validation.maximum_queue_depth,
                timings.tick_nanos,
                timings.query_nanos,
                timings.acceptance_nanos,
                timings.drain_nanos,
                timings.tick_nanos as f64 / event_denominator,
                timings.query_nanos as f64 / validation.queried_haps.max(1) as f64,
                timings.acceptance_nanos as f64 / validation.accepted_events.max(1) as f64,
                timings.drain_nanos as f64 / event_denominator,
                validation.validation_hash,
                validation.counter_hash,
                validation.initial_queue_depth,
                validation.final_queue_depth,
            );
        }
    }
    assert!(
        matched,
        "RUSTEL_SCHEDULER_BENCH_CASE did not name a benchmark workload"
    );
}
