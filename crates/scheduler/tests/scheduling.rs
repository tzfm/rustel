//! The scheduling half.
//!
//! The runtime half (in `rustel-jsruntime`) proves state survives evaluation.
//! This proves the schedule does: a slow or interrupted evaluation must not
//! drain the horizon, duplicate an onset, mix generations, or delay Stop.
//!
//! The clock is virtual, so every assertion is deterministic. The offline
//! `Session::play` loop schedules through the same `VirtualClock`.

use rustel_core::{Pattern, Value, fastcat, pure, stack};
use rustel_fraction::Fraction;
use rustel_scheduler::{
    Clock, Event, Scheduler, SchedulerTickProfile, TickStatus, Transport, VirtualClock,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn atom(s: &str) -> Pattern {
    pure(Value::Str(s.into()))
}
fn seq(a: &str, b: &str) -> Pattern {
    fastcat(vec![atom(a), atom(b)])
}

const CPS: f64 = 1.0;
const HORIZON: f64 = 0.5;

fn setup() -> (Arc<Transport>, Scheduler, VirtualClock) {
    let t = Arc::new(Transport::default());
    let s = Scheduler::new(t.clone(), CPS, HORIZON);
    (t, s, VirtualClock::new(0.0))
}

/// The horizon must actually buffer ahead, and a partly-drained horizon is the
/// real bound on how long evaluation may block querying.
#[test]
fn partly_drained_horizon_reports_what_is_left() {
    let (_t, mut s, clk) = setup();
    s.set_pattern(seq("bd", "sd"), clk.now());
    s.tick(&clk);

    let full = s.horizon_remaining(clk.now());
    assert!(full > 0.0, "horizon did not fill: {full}");

    // Consume part of it. What remains is `H_remaining`: evaluation starting
    // now has only this much cover, not the nominal horizon.
    clk.advance(full * 0.6);
    let _ = s.drain_due(&clk);
    let remaining = s.horizon_remaining(clk.now());

    assert!(
        remaining < full,
        "draining did not reduce the remaining horizon ({full} -> {remaining})"
    );
    assert!(
        remaining > 0.0,
        "horizon fully drained after partial consumption"
    );
}

/// Every onset is emitted exactly once. A refill must never re-emit an onset
/// that was already queued, or a note doubles.
#[test]
fn onsets_are_unique_no_duplicates() {
    let (_t, mut s, clk) = setup();
    s.set_pattern(seq("bd", "sd"), clk.now());

    let mut seen: Vec<(String, String)> = Vec::new();
    let mut ids: Vec<u64> = Vec::new();

    for _ in 0..40 {
        s.tick(&clk);
        for e in s.drain_due(&clk) {
            seen.push((e.whole_begin.show(), e.value.show()));
            ids.push(e.onset_id);
        }
        clk.advance(0.05);
    }

    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "onset ids repeated: {ids:?}");

    let mut positions = seen.clone();
    positions.sort();
    positions.dedup();
    assert_eq!(
        positions.len(),
        seen.len(),
        "the same onset was delivered twice: {seen:?}"
    );
    assert!(!seen.is_empty(), "no events at all");
}

/// Replacing the pattern cancels the old generation. Old and new events must
/// never interleave, and cancelled events are dropped rather than delivered.
#[test]
fn generation_cancellation_without_mixing() {
    let (t, mut s, clk) = setup();
    let g1 = s.set_pattern(seq("old", "old2"), clk.now());
    s.tick(&clk);
    assert!(s.queued() > 0, "generation 1 produced nothing");

    // Re-evaluate before any of it was consumed.
    let g2 = s.set_pattern(seq("new", "new2"), clk.now());
    assert_ne!(g1, g2, "generation must advance on re-evaluation");
    assert_eq!(t.generation(), g2);

    s.tick(&clk);
    clk.advance(HORIZON * 2.0);
    let due = s.drain_due(&clk);

    assert!(!due.is_empty(), "no events after re-evaluation");
    assert!(
        due.iter().all(|e| e.generation == g2),
        "events from a cancelled generation were delivered: {:?}",
        due.iter()
            .map(|e| (e.generation, e.value.show()))
            .collect::<Vec<_>>()
    );
    assert!(
        due.iter().all(|e| e.value.show().starts_with("new")),
        "old-pattern values leaked into the new generation: {:?}",
        due.iter().map(|e| e.value.show()).collect::<Vec<_>>()
    );
}

/// A score-level tempo directive and its graph are one generation commit. The
/// replacement must clear the old lookahead, expose the new `_cps` rate to the
/// query, and use that same rate for target-time spacing.
#[test]
fn pattern_and_tempo_replace_as_one_generation() {
    let (transport, mut scheduler, clock) = setup();
    let first = scheduler.set_pattern(seq("old", "old2"), clock.now());
    assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
    assert!(scheduler.queued() > 0, "old generation did not prefill");

    let second = scheduler.replace_pattern(seq("new", "new2"), 0.125, Some(2.0));
    assert_eq!(second, first + 1, "tempo replacement bumped more than once");
    assert_eq!(transport.generation(), second);
    assert_eq!(scheduler.cps(), 2.0);
    assert_eq!(scheduler.queued(), 0, "old lookahead survived replacement");
    assert_eq!(
        scheduler.horizon_remaining(0.125),
        0.0,
        "replacement did not re-anchor at its install clock"
    );

    assert_eq!(
        scheduler.tick(&VirtualClock::new(0.125)),
        TickStatus::Filled
    );
    let due = scheduler.drain_through(&VirtualClock::new(0.125), 0.625);
    assert_eq!(
        due.iter()
            .map(|event| (event.value.show(), event.target_time))
            .collect::<Vec<_>>(),
        [("new2".into(), 0.125), ("new".into(), 0.375)],
        "replacement target spacing did not use two cycles per second"
    );
    assert!(due.iter().all(|event| event.generation == second));

    let third = scheduler.replace_pattern(seq("kept", "rate"), 0.625, None);
    assert_eq!(third, second + 1);
    assert_eq!(
        scheduler.cps(),
        2.0,
        "a replacement without tempo reset the current rate"
    );
}

#[test]
fn lookahead_transfer_is_future_bounded_without_advancing_staleness() {
    let (_t, mut scheduler, clock) = setup();
    scheduler.set_pattern(
        fastcat(vec![atom("a"), atom("b"), atom("c"), atom("d")]),
        clock.now(),
    );
    assert_eq!(scheduler.tick(&clock), TickStatus::Filled);

    let first = scheduler.drain_through(&clock, 0.1);
    assert!(!first.is_empty(), "lookahead transferred no initial onset");
    assert!(first.iter().all(|event| event.target_time <= 0.1));
    let queued_after_first = scheduler.queued();
    assert!(
        queued_after_first > 0,
        "lookahead drained past its deadline"
    );

    let second = scheduler.drain_through(&clock, HORIZON);
    assert!(
        !second.is_empty(),
        "remaining lookahead was not transferred"
    );
    assert!(second.iter().all(|event| event.target_time <= HORIZON));
    assert!(
        first
            .iter()
            .all(|old| second.iter().all(|new| old.onset_id != new.onset_id)),
        "lookahead transfer duplicated an onset"
    );
}

/// Stop is immediate and independent of the event queue - never stuck behind
/// an evaluation in progress.
#[test]
fn stop_is_immediate_and_independent() {
    let (t, mut s, clk) = setup();
    s.set_pattern(seq("bd", "sd"), clk.now());
    s.tick(&clk);
    assert!(s.queued() > 0);

    let before = Instant::now();
    t.stop();
    let latency = before.elapsed();
    assert!(
        latency < Duration::from_millis(1),
        "Stop took {latency:?} - it must not traverse the event queue"
    );

    clk.advance(HORIZON * 2.0);
    let due = s.drain_due(&clk);
    assert!(
        due.is_empty(),
        "events were delivered after Stop: {:?}",
        due.iter().map(|e| e.value.show()).collect::<Vec<_>>()
    );
    assert_eq!(s.tick(&clk), TickStatus::Stopped, "tick must respect Stop");

    // And it resumes cleanly.
    t.start();
    assert_ne!(s.tick(&clk), TickStatus::Stopped);
}

/// If evaluation blocks querying for longer than the stale limit, the
/// transport degrades rather than delivering ever-staler events.
#[test]
fn stale_events_expire_rather_than_replaying() {
    let (_t, mut s, clk) = setup();
    s.set_stale_limit(0.25);
    s.set_pattern(seq("bd", "sd"), clk.now());
    s.tick(&clk);
    let queued = s.queued();
    assert!(queued > 0);

    // Simulate evaluation blocking the query thread well past the limit.
    clk.advance(1.0);
    let due = s.drain_due(&clk);
    assert!(
        due.is_empty(),
        "stale events were delivered {} s late instead of expiring: {:?}",
        1.0,
        due.iter().map(|e| e.value.show()).collect::<Vec<_>>()
    );

    // Once querying resumes, playback recovers.
    s.tick(&clk);
    clk.advance(0.05);
    let recovered = s.drain_due(&clk);
    assert!(
        !recovered.is_empty(),
        "transport did not recover after the stall"
    );
}

/// A blocked query thread must not silently drain the horizon: with the
/// generation cancelled and no refill, nothing from the old schedule survives.
#[test]
fn evaluation_stall_does_not_replay_consumed_onsets() {
    let (_t, mut s, clk) = setup();
    s.set_pattern(seq("bd", "sd"), clk.now());
    s.tick(&clk);

    clk.advance(0.2);
    let first = s.drain_due(&clk);
    let first_ids: Vec<u64> = first.iter().map(|e| e.onset_id).collect();

    // Stall, then resume and refill.
    clk.advance(0.1);
    s.tick(&clk);
    clk.advance(0.2);
    let second = s.drain_due(&clk);

    for e in &second {
        assert!(
            !first_ids.contains(&e.onset_id),
            "onset {} was delivered twice across a stall",
            e.onset_id
        );
    }
}

// ---------------------------------------------------------------------------
// The measured inequality:
//   D + interrupt_recovery + next_query_p99.9 + wake_jitter < H_remaining
// ---------------------------------------------------------------------------

/// Measure each term on THIS machine and assert the budget closes with margin.
///
/// `D` is derived from the others and then **discounted by a safety factor**,
/// never set to the calculated ceiling: the measurements are a floor from a
/// finite sample on an idle machine, so the first sample past the measured
/// tail would overrun and drop audio.
#[test]
fn deadline_budget_fits_the_remaining_horizon() {
    use rustel_jsruntime::{JsRuntime, Slot};

    /// p99.9 needs ~1-in-1000 resolution, so 200 samples cannot establish it -
    /// the "max of 200" it reports is really about p99.5 with one observation.
    /// 20_000 puts 20 samples in the tail.
    const QUERY_SAMPLES: usize = 20_000;
    const RECOVERY_SAMPLES: usize = 50;
    const WAKE_SAMPLES: usize = 2_000;

    fn percentile(sorted: &[Duration], p: f64) -> Duration {
        let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
        sorted[idx]
    }

    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => (x) => x + '!'");
    rt.set_active(&b, seq("bd", "sd").fmap_js(id)).unwrap();

    // --- interrupt_recovery: interrupt, then time until the runtime is usable
    let mut recovery: Vec<Duration> = Vec::with_capacity(RECOVERY_SAMPLES);
    for _ in 0..RECOVERY_SAMPLES {
        let _ = rt.with_deadline(Duration::from_millis(5), || rt.eval("while (true) {}"));
        let t0 = Instant::now();
        let _ = rt
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap();
        recovery.push(t0.elapsed());
    }
    recovery.sort();
    let recovery_p999 = percentile(&recovery, 0.999);

    // --- next_query latency distribution
    let mut queries: Vec<Duration> = Vec::with_capacity(QUERY_SAMPLES);
    for _ in 0..QUERY_SAMPLES {
        let t0 = Instant::now();
        let _ = rt
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap();
        queries.push(t0.elapsed());
    }
    queries.sort();
    let query_p50 = percentile(&queries, 0.50);
    let query_p999 = percentile(&queries, 0.999);
    let query_max = *queries.last().unwrap();

    // Measure wake jitter through the condition-variable path. A plain
    // `thread::sleep` would only measure timer granularity. The waiter asks
    // for each signal. Two signals merged into one wake would leave a
    // timestamp in the channel, and each later sample would read its age.
    let waker = Arc::new(rustel_scheduler::Waker::default());
    let mut jitter: Vec<Duration> = Vec::with_capacity(WAKE_SAMPLES);
    {
        let w = waker.clone();
        let (ask, asked) = std::sync::mpsc::channel::<()>();
        let (tx, rx) = std::sync::mpsc::channel::<Instant>();
        let producer = std::thread::spawn(move || {
            while asked.recv().is_ok() {
                std::thread::sleep(Duration::from_micros(50));
                tx.send(Instant::now()).unwrap();
                w.signal();
            }
        });
        for _ in 0..WAKE_SAMPLES {
            ask.send(()).unwrap();
            assert!(
                waker.wait(Duration::from_secs(5)),
                "the producer sent no signal"
            );
            jitter.push(rx.recv().unwrap().elapsed());
        }
        drop(ask);
        producer.join().unwrap();
    }
    jitter.sort();
    let wake_p999 = percentile(&jitter, 0.999);

    // --- H_remaining on a partly drained horizon
    let (_t, mut s, clk) = setup();
    s.set_pattern(seq("bd", "sd"), clk.now());
    s.tick(&clk);
    let full = s.horizon_remaining(clk.now());
    clk.advance(full * 0.5);
    let _ = s.drain_due(&clk);
    let h_remaining = s.horizon_remaining(clk.now());

    let overhead = recovery_p999 + query_p999 + wake_p999;
    let deadline = s.affordable_deadline(clk.now(), overhead);

    println!(
        "scheduler timing budget on {} ({} arch)\n           samples ............ query {QUERY_SAMPLES}, wake {WAKE_SAMPLES}, recovery {RECOVERY_SAMPLES}\n           H_remaining ........ {:?}\n           interrupt_recovery . p99.9 {:?}\n           query .............. p50 {:?}  p99.9 {:?}  max {:?}\n           wake_jitter ........ p99.9 {:?} (condvar path)\n           overhead ........... {:?}\n           safety factor ...... {}x\n           => D (afforded) .... {:?}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        Duration::from_secs_f64(h_remaining),
        recovery_p999,
        query_p50,
        query_p999,
        query_max,
        wake_p999,
        overhead,
        rustel_scheduler::Scheduler::BUDGET_SAFETY_FACTOR,
        deadline,
    );

    let d = deadline.unwrap_or_else(|| {
        panic!(
            "budget does not close on {}/{}: overhead {:?} x{} safety >= H_remaining {:?}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            overhead,
            rustel_scheduler::Scheduler::BUDGET_SAFETY_FACTOR,
            Duration::from_secs_f64(h_remaining)
        )
    });
    assert!(
        d > Duration::from_millis(1),
        "afforded deadline {d:?} leaves no usable evaluation budget"
    );
    rt.clear_active();
}

/// The scheduler must REFUSE an evaluation it cannot cover, rather than start
/// one that will overrun the horizon.
#[test]
fn evaluation_is_deferred_when_the_horizon_cannot_cover_it() {
    let (_t, mut s, clk) = setup();
    s.set_pattern(seq("bd", "sd"), clk.now());
    s.tick(&clk);

    // Plenty of horizon: a small budget is affordable.
    let small = Duration::from_micros(100);
    assert!(
        s.affordable_deadline(clk.now(), small).is_some(),
        "a tiny budget should be affordable with a full horizon"
    );

    // Drain almost all of it: the same budget no longer fits.
    let full = s.horizon_remaining(clk.now());
    clk.advance(full * 0.999);
    let _ = s.drain_due(&clk);
    assert!(
        s.affordable_deadline(clk.now(), Duration::from_millis(50))
            .is_none(),
        "scheduler offered a deadline it cannot cover - evaluation must be \
         DEFERRED when H_remaining is exhausted, not started and overrun"
    );
}

/// The scheduler bounds one tick's allocation itself. A cap that the caller
/// applies to the finished timeline runs after the allocation.
#[test]
fn a_dense_tick_never_queues_past_the_bound() {
    let transport = Arc::new(Transport::default());
    // A huge horizon forces ONE tick to cover an enormous span, which is the
    // shape the bound exists for: the fill loop is where the growth happens.
    let mut scheduler = Scheduler::new(transport.clone(), 0.5, 1_000_000.0);
    let clock = VirtualClock::new(0.0);

    // 32 onsets per cycle over a million seconds of horizon: far past the cap.
    let dense = fastcat((0..32).map(|i| atom(&format!("x{i}"))).collect());
    scheduler.set_pattern(dense, clock.now());

    // One tick must not query the whole horizon: the span is clamped before
    // `query_state` runs. A cap on the drain alone still allocates every hap
    // first. With the span clamped to 4 cycles, one tick queues 4 * 32 = 128.
    let status = scheduler.tick(&clock);
    assert_eq!(
        status,
        TickStatus::Filled,
        "a clamped tick fills normally; reaching the queue cap in ONE tick \
         means the whole horizon was queried at once"
    );
    // Exact, not an upper bound: 4 cycles of a 32-per-cycle pattern is 128
    // onsets, so a change to the clamp must fail here.
    let queued = scheduler.queued();
    assert_eq!(
        queued, 128,
        "one tick queued {queued} events, not the 128 a 4-cycle clamp \
         produces for a 32-per-cycle pattern. Either the per-tick query span is \
         no longer clamped to 4 - in which case `query_state` materialises \
         more of the horizon in one allocation - or the clamp changed and this \
         expectation needs re-deriving deliberately."
    );
    // Repeated ticking must stay bounded too - the clamp makes the horizon
    // fill over several ticks rather than one, and that must not accumulate
    // without limit either.
    for _ in 0..50 {
        scheduler.tick(&clock);
    }
    assert!(
        scheduler.queued() <= rustel_scheduler::MAX_QUEUED_EVENTS,
        "repeated ticks grew the queue to {}",
        scheduler.queued()
    );
    assert!(
        scheduler.queued() > queued,
        "the horizon must keep filling across ticks; clamping the span must \
         slow the fill, not stop it"
    );
}

/// A queue-full fill holds the cursor at its earliest unqueued onset, a tick at
/// the cap makes no query, and a caller that drains only after a tick that was
/// not `QueueFull` (as the live session does) still receives every onset of the
/// span exactly once.
#[test]
fn a_queue_full_tick_drops_no_onsets_permanently() {
    let transport = Arc::new(Transport::default());
    // A huge horizon keeps the fill gate open, as in the bound test above.
    let mut scheduler = Scheduler::new(transport, 0.5, 1_000_000.0);
    let clock = VirtualClock::new(0.0);
    // 1600 stacked 32-onset cycles over the 4-cycle span clamp: 204_800 onsets.
    let cycle = fastcat((0..32).map(|i| atom(&format!("x{i}"))).collect());
    scheduler.set_pattern(stack(vec![cycle; 1600]), clock.now());

    assert_eq!(scheduler.tick(&clock), TickStatus::QueueFull);
    assert_eq!(
        scheduler.queued(),
        rustel_scheduler::MAX_QUEUED_EVENTS,
        "the fill must stop at the cap"
    );
    let cut = scheduler.scheduled_to_cycle();
    assert!(
        cut < 4.0,
        "the cursor consumed the whole span ({cut} cycles) although \
         {} onsets were never queued",
        204_800 - rustel_scheduler::MAX_QUEUED_EVENTS
    );

    let mut status = scheduler.tick(&clock);
    assert_eq!(
        status,
        TickStatus::HorizonFull,
        "a tick at the cap must leave the caller free to drain"
    );
    assert_eq!(scheduler.scheduled_to_cycle(), cut);

    // The clock never moves, so the whole span is due now.
    let mut voices: std::collections::BTreeMap<(String, String), usize> =
        std::collections::BTreeMap::new();
    let mut delivered = 0usize;
    for _ in 0..8 {
        if status != TickStatus::QueueFull {
            for event in scheduler.drain_through(&clock, 8.0) {
                if event.whole_begin.to_f64() < 4.0 {
                    *voices
                        .entry((event.whole_begin.show(), event.value.show()))
                        .or_default() += 1;
                    delivered += 1;
                }
            }
        }
        if scheduler.scheduled_to_cycle() >= 4.0 {
            break;
        }
        status = scheduler.tick(&clock);
    }
    assert_eq!(
        delivered, 204_800,
        "the queue cap must defer onsets, not drop them"
    );
    assert_eq!(
        voices.len(),
        128,
        "four cycles of a 32-onset pattern must all be present"
    );
    assert!(
        voices.values().all(|&count| count == 1600),
        "a stacked voice was dropped or doubled"
    );
}

#[test]
fn a_caller_can_tighten_one_atomic_tick_to_one_cycle() {
    let transport = Arc::new(Transport::default());
    let mut scheduler = Scheduler::new(transport, 100.0, 0.5);
    let clock = VirtualClock::new(0.0);
    let dense = fastcat((0..32).map(|i| atom(&format!("x{i}"))).collect());
    scheduler.set_pattern(dense, clock.now());

    assert_eq!(
        scheduler.tick_with_max_span_cycles(&clock, 1.0),
        TickStatus::Filled
    );
    assert_eq!(
        scheduler.queued(),
        32,
        "the caller's one-cycle atomic bound was ignored"
    );
    assert!(
        (scheduler.horizon_remaining(clock.now()) - 0.01).abs() < 1e-9,
        "one 100-CPS cycle should add exactly 10 ms of cover"
    );
}

/// ...and an ordinary pattern is nowhere near it, so the bound never fires in
/// normal use.
#[test]
fn an_ordinary_pattern_stays_far_below_the_queue_bound() {
    let transport = Arc::new(Transport::default());
    let mut scheduler = Scheduler::new(transport.clone(), 0.5, 0.2);
    let clock = VirtualClock::new(0.0);
    scheduler.set_pattern(seq("bd", "sd"), clock.now());
    assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
    assert!(scheduler.queued() < 100, "queued {}", scheduler.queued());
}

/// Identical stacked haps remain distinct voices without replaying on refill.
#[test]
fn identical_stacked_voices_are_not_deduplicated() {
    let (_t, mut s, clk) = setup();
    s.set_pattern(stack(vec![atom("bd"), atom("bd")]), clk.now());

    let mut per_cycle: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut ids: Vec<u64> = Vec::new();
    for _ in 0..40 {
        s.tick(&clk);
        for e in s.drain_due(&clk) {
            *per_cycle.entry(e.whole_begin.show()).or_default() += 1;
            ids.push(e.onset_id);
        }
        clk.advance(0.05);
    }

    assert!(!per_cycle.is_empty(), "no events at all");
    for (begin, count) in &per_cycle {
        assert_eq!(
            *count, 2,
            "cycle {begin} delivered {count} voices, want exactly 2 (per_cycle: {per_cycle:?})"
        );
    }

    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "onset ids repeated: {ids:?}");
}

/// A continued replacement suppresses each onset the outgoing generation
/// already emitted in `[from, takeover)`: the device plays that copy until
/// the takeover. From the takeover on, the new generation emits every onset.
#[test]
fn continued_replacement_suppresses_unchanged_music_inside_the_takeover_window() {
    let (_t, mut s, clk) = setup();
    // Four onsets per cycle on a 0.25 grid.
    let grid = || fastcat(vec![atom("a"), atom("b"), atom("c"), atom("d")]);
    s.set_pattern(grid(), clk.now());
    assert_eq!(s.tick(&clk), TickStatus::Filled);

    // The producer's prefetch: at clock 2.5 the cursor runs to 3.0.
    clk.advance(2.5);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let _ = s.drain_due(&clk);
    assert_eq!(
        s.scheduled_to_cycle(),
        3.0,
        "test premise: prefetched to 3.0"
    );

    // The edit lands at 2.6; the takeover half a cycle later.
    clk.advance(0.1);
    let from = 2.6;
    let _ = s.replace_pattern_continued(grid(), clk.now(), None, from, from + 0.5);
    assert_eq!(
        s.scheduled_to_cycle(),
        2.6,
        "the continuation re-queries from the edit instant, not the takeover"
    );

    assert_eq!(s.tick(&clk), TickStatus::Filled);
    clk.advance(0.45);
    let begins: Vec<f64> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| e.whole_begin.to_f64())
        .collect();
    assert_eq!(
        begins,
        vec![3.0],
        "the old 2.75 onset stays suppressed (the device plays its own copy \
         until the takeover) and the never-emitted 3.0 comes through: {begins:?}"
    );

    // Past the takeover the new generation is the only source again.
    clk.advance(0.45);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let begins: Vec<f64> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| e.whole_begin.to_f64())
        .collect();
    assert_eq!(
        begins,
        vec![3.25, 3.5],
        "onsets past the takeover are emitted normally: {begins:?}"
    );
}

/// A continued replacement suppresses every unchanged onset in
/// `[from, takeover)` even when a dense score prunes the emitted set on every
/// fill and the query frontier stands a full horizon (10 cycles at cps 20)
/// past the edit instant.
#[test]
fn continued_replacement_keeps_its_overlap_marks_through_a_prune() {
    let t = Arc::new(Transport::default());
    let mut s = Scheduler::new(t, 20.0, HORIZON);
    let clk = VirtualClock::new(0.0);
    // 512 onsets per cycle keep the emitted set over the prune threshold, so
    // every fill prunes.
    let grid = || fastcat(vec![atom("g"); 512]);
    s.set_pattern(grid(), clk.now());

    // Play 1.2 s (24 cycles), draining due onsets as the producer does.
    for _ in 0..12 {
        clk.advance(0.1);
        assert_eq!(s.tick(&clk), TickStatus::Filled);
        let _ = s.drain_due(&clk);
    }

    // The edit lands half a grid step into cycle 24 and the takeover one cycle
    // later; the outgoing generation already emitted every onset in between.
    clk.advance(0.05 / 1024.0);
    let from = clk.now() * 20.0;
    let takeover = from + 1.0;
    assert!(
        s.scheduled_to_cycle() >= from + 9.5,
        "test premise: the frontier stands a full horizon past the edit \
         instant ({} vs {from})",
        s.scheduled_to_cycle()
    );
    let _ = s.replace_pattern_continued(grid(), clk.now(), None, from, takeover);

    assert_eq!(s.tick(&clk), TickStatus::Filled);
    clk.advance(0.06);
    let begins: Vec<f64> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| e.whole_begin.to_f64())
        .collect();
    let re_emitted = begins.iter().filter(|&&b| b < takeover - 1e-9).count();
    assert_eq!(
        re_emitted,
        0,
        "{re_emitted} onsets inside the takeover window were emitted twice \
         (first of {}: {:?})",
        begins.len(),
        begins.first()
    );
    assert!(
        !begins.is_empty(),
        "onsets past the takeover must still be emitted"
    );
}

/// A hap the edit adds inside the takeover window has no old-generation key.
/// The scheduler emits it, and the device admits it late.
#[test]
fn continued_replacement_emits_a_newly_added_onset_inside_the_window() {
    let (_t, mut s, clk) = setup();
    // Onsets on the half-beats only.
    let old = || fastcat(vec![atom("a"), atom("c")]);
    s.set_pattern(old(), clk.now());
    assert_eq!(s.tick(&clk), TickStatus::Filled);

    // Prefetch to 3.0, then the edit lands at 2.6, takeover at 3.1.
    clk.advance(2.5);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let _ = s.drain_due(&clk);

    clk.advance(0.1);
    let from = 2.6;
    // The edit fills in the off-beats: the 2.75 onset is new.
    let new = fastcat(vec![atom("a"), atom("b"), atom("c"), atom("d")]);
    let _ = s.replace_pattern_continued(new, clk.now(), None, from, from + 0.5);

    assert_eq!(s.tick(&clk), TickStatus::Filled);
    clk.advance(0.45);
    let delivered: Vec<(f64, String)> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| (e.whole_begin.to_f64(), e.value.show()))
        .collect();
    assert!(
        delivered.contains(&(2.75, "d".into())),
        "the newly-added 2.75 onset must be emitted (late) - dropping it is \
         exactly the bug this fixes: {delivered:?}"
    );
    assert!(
        delivered.contains(&(3.0, "a".into())),
        "the onset the old cursor stopped on has no old copy and is emitted: \
         {delivered:?}"
    );
}

/// A hap the edit changes (same `begin`, different value) is not emitted
/// inside the window: the device still plays the outgoing copy, and the two
/// would sound together. The change sounds from the takeover on.
#[test]
fn continued_replacement_leaves_a_changed_onset_to_the_outgoing_copy() {
    let (_t, mut s, clk) = setup();
    // Three onsets per cycle at thirds: 0, 1/3, 2/3.
    let old = || fastcat(vec![atom("1"), atom("2"), atom("3")]);
    s.set_pattern(old(), clk.now());
    assert_eq!(s.tick(&clk), TickStatus::Filled);

    // Prefetch to 3.0 and drain through the cover, as the producer's
    // `schedule_through` does: the device holds every old onset before 3.0.
    // The edit at 2.6 and the takeover at 3.1 put the old 8/3 in the window.
    clk.advance(2.5);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let _ = s.drain_through(&clk, 3.0);

    clk.advance(0.1);
    let from = 2.6;
    // The 8/3 onset changes from 3 to 5 at the same begin. The other onsets
    // keep their values.
    let new = fastcat(vec![atom("1"), atom("2"), atom("5")]);
    let _ = s.replace_pattern_continued(new, clk.now(), None, from, from + 0.5);

    assert_eq!(s.tick(&clk), TickStatus::Filled);
    clk.advance(0.45);
    let delivered: Vec<(String, String)> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| (e.whole_begin.show(), e.value.show()))
        .collect();
    assert_eq!(
        delivered,
        vec![("3/1".into(), "1".into())],
        "the device plays its own copy of 8/3 until the takeover: neither \
         rendering comes through the scheduler, and the onset the old cursor \
         stopped on does: {delivered:?}"
    );

    // Past the takeover the replacement is the only source again: cycle 3's
    // 10/3 ("2") comes due first.
    clk.advance(0.45);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let delivered: Vec<(String, String)> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| (e.whole_begin.show(), e.value.show()))
        .collect();
    assert_eq!(
        delivered,
        vec![("10/3".into(), "2".into())],
        "past the takeover everything is emitted normally: {delivered:?}"
    );

    // And the change is heard from there on, on the same grid.
    clk.advance(0.6);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let delivered: Vec<(String, String)> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| (e.whole_begin.show(), e.value.show()))
        .collect();
    assert_eq!(
        delivered,
        vec![("11/3".into(), "5".into()), ("4/1".into(), "1".into())],
        "the changed onset sounds past the takeover: {delivered:?}"
    );
}

/// The controls of one sound, as a hap value.
fn sound(controls: &[(&str, &str)]) -> Pattern {
    pure(Value::object(controls.iter().map(|(name, value)| {
        ((*name).to_owned(), Value::Str((*value).to_owned()))
    })))
}

/// Four of `pattern` to the cycle, on the 0.25 grid.
fn four(pattern: Pattern) -> Pattern {
    fastcat(vec![pattern; 4])
}

/// The outgoing generation at clock 2.5, prefetched to 3.0 and handed to the
/// device; the edit at 2.6 with its takeover at 3.1 puts the old 2.75 onsets
/// inside the window. Returns what the replacement delivers of the window
/// and of the first onset past the old cursor, as `(begin, value)`.
fn window_of_a_continued_replacement(old: Pattern, new: Pattern) -> Vec<(f64, String)> {
    let (_t, mut s, clk) = setup();
    s.set_pattern(old, clk.now());
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    clk.advance(2.5);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let _ = s.drain_through(&clk, 3.0);

    clk.advance(0.1);
    let from = 2.6;
    let _ = s.replace_pattern_continued(new, clk.now(), None, from, from + 0.5);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    clk.advance(0.45);
    s.drain_due(&clk)
        .into_iter()
        .map(|e| (e.whole_begin.to_f64(), e.value.show()))
        .collect()
}

/// Every note of a chord gains a control. Each changed note has its outgoing
/// copy on the device, so none is emitted inside the window and the chord
/// does not sound twice.
#[test]
fn continued_replacement_leaves_a_changed_chord_to_its_outgoing_copies() {
    let chord = |extra: &[(&str, &str)]| {
        stack(
            ["e1", "g2", "c3"]
                .into_iter()
                .map(|note| {
                    let mut controls = vec![("s", "supersaw"), ("note", note)];
                    controls.extend_from_slice(extra);
                    four(sound(&controls))
                })
                .collect(),
        )
    };
    let delivered = window_of_a_continued_replacement(chord(&[]), chord(&[("unison", "5")]));
    let inside: Vec<_> = delivered.iter().filter(|(begin, _)| *begin < 3.0).collect();
    assert!(
        inside.is_empty(),
        "the device already plays the 2.75 chord: {inside:?}"
    );
    assert_eq!(
        delivered.iter().filter(|(begin, _)| *begin == 3.0).count(),
        3,
        "the chord the old cursor stopped on is the replacement's: {delivered:?}"
    );
}

/// An onset the edit adds on a begin the outgoing generation also plays is
/// emitted. The unchanged hap beside it matches its own copy exactly, in
/// either query order, so no copy is left for the added hap to take.
#[test]
fn continued_replacement_emits_an_onset_added_beside_an_unchanged_one() {
    let delivered = window_of_a_continued_replacement(
        four(atom("bd")),
        stack(vec![four(atom("hh")), four(atom("bd"))]),
    );
    let inside: Vec<_> = delivered
        .iter()
        .filter(|(begin, _)| *begin < 3.0)
        .cloned()
        .collect();
    assert_eq!(
        inside,
        vec![(2.75, "hh".to_owned())],
        "the added hat is emitted and the kick keeps its copy: {delivered:?}"
    );
}

/// An edit changes a hap and adds one on the same begin. The outgoing copy
/// stands in for the hap most like it, and the added hap is emitted, although
/// the added lane comes first in the query.
#[test]
fn continued_replacement_emits_the_added_onset_beside_a_changed_one() {
    let delivered = window_of_a_continued_replacement(
        four(sound(&[("s", "bd")])),
        stack(vec![
            four(sound(&[("s", "sawtooth"), ("note", "c2")])),
            four(sound(&[("s", "bd"), ("gain", "0.5")])),
        ]),
    );
    let inside: Vec<_> = delivered
        .iter()
        .filter(|(begin, _)| *begin < 3.0)
        .cloned()
        .collect();
    assert_eq!(
        inside,
        vec![(2.75, "s:sawtooth note:c2".to_owned())],
        "the added note is emitted and the changed kick keeps the outgoing \
         copy: {delivered:?}"
    );
}

/// A hap the edit moves in time (the same value 1/50 cycle later) is not
/// emitted inside the window: its outgoing copy, on another begin, stands in
/// for it. The moved onset past the old cursor has no copy and is emitted.
#[test]
fn continued_replacement_leaves_a_moved_onset_to_its_outgoing_copy() {
    let delivered = window_of_a_continued_replacement(
        four(atom("bd")),
        four(atom("bd")).late(Fraction::new(1, 50)),
    );
    assert_eq!(
        delivered,
        vec![(3.02, "bd".to_owned())],
        "the device plays its 2.75 copy in place of the moved 2.77: {delivered:?}"
    );
}

/// A moved hap takes its own outgoing copy before a different hap on the
/// copy's begin can. The hat moves and a snare is added where the hat was:
/// the snare is emitted and the moved hat is not.
#[test]
fn continued_replacement_emits_an_onset_added_where_a_moved_one_was() {
    let delivered = window_of_a_continued_replacement(
        four(atom("hh")),
        stack(vec![
            four(atom("sd")),
            four(atom("hh")).late(Fraction::new(1, 50)),
        ]),
    );
    let inside: Vec<_> = delivered
        .iter()
        .filter(|(begin, _)| *begin < 3.0)
        .cloned()
        .collect();
    assert_eq!(
        inside,
        vec![(2.75, "sd".to_owned())],
        "the added snare is emitted and the moved hat keeps its copy: {delivered:?}"
    );
}

/// A fill that the queue cap cuts short inside the takeover window queries
/// its unqueued onsets again. Each outgoing copy still stands in for exactly
/// one changed hap: one changed lane is before the cap and one is after it.
#[test]
fn a_queue_full_fill_spends_each_outgoing_copy_once() {
    let transport = Arc::new(Transport::default());
    // A huge horizon keeps the fill gate open, as in the bound tests above.
    let mut scheduler = Scheduler::new(transport, 0.5, 1_000_000.0);
    let clock = VirtualClock::new(0.0);
    let lane = |controls: &[(&str, &str)]| fastcat(vec![sound(controls); 32]);
    scheduler.set_pattern(
        stack(vec![
            lane(&[("s", "bd"), ("n", "1"), ("room", "0.2")]),
            lane(&[("s", "bd"), ("n", "2"), ("room", "0.2")]),
        ]),
        clock.now(),
    );
    assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
    let _ = scheduler.drain_through(&clock, 8.0);

    // 204_800 added onsets over the 4-cycle span clamp, so the cap cuts the
    // first pass inside the window [0.25, 2.75).
    let first = [("s", "bd"), ("n", "1"), ("room", "0.2"), ("gain", "0.5")];
    let second = [("s", "bd"), ("n", "3")];
    let mut lanes = vec![lane(&first)];
    lanes.extend(vec![lane(&[("s", "hh")]); 1600]);
    lanes.push(lane(&second));
    let _ = scheduler.replace_pattern_continued(stack(lanes), clock.now(), None, 0.25, 2.75);

    let mut status = scheduler.tick(&clock);
    assert_eq!(status, TickStatus::QueueFull);
    let cut = scheduler.scheduled_to_cycle();
    assert!(
        (0.25..2.75).contains(&cut),
        "test premise: the cap falls inside the window, got {cut}"
    );

    let mut voices: std::collections::BTreeMap<(String, String), usize> =
        std::collections::BTreeMap::new();
    for _ in 0..8 {
        if status != TickStatus::QueueFull {
            for event in scheduler.drain_through(&clock, 16.0) {
                *voices
                    .entry((event.whole_begin.show(), event.value.show()))
                    .or_default() += 1;
            }
        }
        if scheduler.scheduled_to_cycle() >= 4.25 {
            break;
        }
        status = scheduler.tick(&clock);
    }
    let count = |begin: Fraction, value: &str| {
        voices
            .get(&(begin.show(), value.to_owned()))
            .copied()
            .unwrap_or(0)
    };
    for step in 8..136 {
        let begin = Fraction::new(step, 32);
        let changed = usize::from(begin.to_f64() >= 2.75);
        assert_eq!(
            (
                count(begin, "s:hh"),
                count(begin, "s:bd n:1 room:0.2 gain:0.5"),
                count(begin, "s:bd n:3"),
            ),
            (1600, changed, changed),
            "at {}: every added hat once, and the changed drums only past \
             the takeover",
            begin.show()
        );
    }
}

/// The window ends AT the takeover, mirroring the device contract: old events
/// at or past the takeover frame are retired when the generation flips, so
/// their replacements must be emitted even though the old generation emitted
/// them too. An old onset before the takeover is kept by the device and stays
/// suppressed.
#[test]
fn continued_replacement_carries_only_before_the_takeover() {
    let (_t, mut s, clk) = setup();
    let grid = || fastcat(vec![atom("a"), atom("b"), atom("c"), atom("d")]);
    s.set_pattern(grid(), clk.now());
    assert_eq!(s.tick(&clk), TickStatus::Filled);

    // The producer prefetched far: at clock 3.2 the cursor runs to 3.7.
    clk.advance(3.2);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let _ = s.drain_due(&clk);
    assert_eq!(
        s.scheduled_to_cycle(),
        3.7,
        "test premise: prefetched to 3.7"
    );

    // Edit at 3.3, takeover at 3.8: the old 3.5 is inside the window and kept
    // by the device; 3.75 the old generation never emitted; 4.0 sits past the
    // takeover where the old copy would be retired.
    clk.advance(0.1);
    let from = 3.3;
    let _ = s.replace_pattern_continued(grid(), clk.now(), None, from, from + 0.5);

    assert_eq!(s.tick(&clk), TickStatus::Filled);
    clk.advance(0.45);
    let begins: Vec<f64> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| e.whole_begin.to_f64())
        .collect();
    assert_eq!(
        begins,
        vec![3.75],
        "3.5 is the old copy's to play and stays suppressed; 3.75 was never \
         emitted by the old generation and must be: {begins:?}"
    );

    // Past the takeover, including where the old generation HAD emitted.
    clk.advance(0.3);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let begins: Vec<f64> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| e.whole_begin.to_f64())
        .collect();
    assert_eq!(
        begins,
        vec![4.0],
        "the onset past the takeover is emitted normally - and had the old \
         generation emitted it too, the device would retire that copy at the \
         takeover, so emitting it here is the only correct outcome: {begins:?}"
    );
}

/// A zero continuity margin (the takeover on the edit instant) still
/// continues: nothing is suppressed, and the mapping keeps the timeline's
/// phase. The plain far-cursor contract would shift the whole grid.
#[test]
fn continued_replacement_with_a_zero_margin_window_keeps_the_grid() {
    let (_t, mut s, clk) = setup();
    let grid = || fastcat(vec![atom("a"), atom("b"), atom("c"), atom("d")]);
    s.set_pattern(grid(), clk.now());
    assert_eq!(s.tick(&clk), TickStatus::Filled);

    // Prefetch past the edit instant, as any producer that queried ahead does.
    clk.advance(2.5);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let _ = s.drain_due(&clk);
    assert_eq!(
        s.scheduled_to_cycle(),
        3.0,
        "test premise: prefetched to 3.0"
    );

    // Edit at 2.6 with the takeover on the edit instant, a zero continuity
    // margin: no overlap window, nothing to suppress, nothing pre-marked.
    // The mapping must still not change.
    clk.advance(0.1);
    let from = 2.6;
    let _ = s.replace_pattern_continued(grid(), clk.now(), None, from, from);
    assert_eq!(
        s.scheduled_to_cycle(),
        from,
        "the continuation re-queries from the edit instant even when the window is empty"
    );

    assert_eq!(s.tick(&clk), TickStatus::Filled);
    clk.advance(0.45);
    let begins: Vec<f64> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| e.whole_begin.to_f64())
        .collect();
    assert_eq!(
        begins,
        vec![2.75, 3.0],
        "the grid keeps its phase: onsets land on the original 0.25 grid, not \
         on a grid re-anchored at the far cursor: {begins:?}"
    );

    // The phase holds past the (nonexistent) takeover too.
    clk.advance(0.45);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let begins: Vec<f64> = s
        .drain_due(&clk)
        .into_iter()
        .map(|e| e.whole_begin.to_f64())
        .collect();
    assert_eq!(
        begins,
        vec![3.25, 3.5],
        "the grid stays on its original phase: {begins:?}"
    );
}

/// The old generation prefetched to 3.0 on a 0.25 grid, and the edit lands
/// at 2.6: the state every takeover-edge test below starts from.
fn prefetched_to_three() -> (Scheduler, VirtualClock, impl Fn() -> Pattern) {
    let (_t, mut s, clk) = setup();
    let grid = || fastcat(vec![atom("a"), atom("b"), atom("c"), atom("d")]);
    s.set_pattern(grid(), clk.now());
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    clk.advance(2.5);
    assert_eq!(s.tick(&clk), TickStatus::Filled);
    let _ = s.drain_due(&clk);
    assert_eq!(
        s.scheduled_to_cycle(),
        3.0,
        "test premise: prefetched to 3.0"
    );
    clk.advance(0.1);
    (s, clk, grid)
}

fn begins_through(s: &mut Scheduler, clk: &VirtualClock, through: f64) -> Vec<f64> {
    assert_eq!(s.tick(clk), TickStatus::Filled);
    s.drain_through(clk, through)
        .into_iter()
        .map(|e| e.whole_begin.to_f64())
        .collect()
}

/// The onset at 2.75 is 0.3 frame (at 48 kHz) after the takeover edge, so the
/// device drops its outgoing copy. The window ends on the edge and leaves the
/// onset to the replacement, which emits it.
#[test]
fn continued_replacement_until_an_edge_emits_the_onset_just_after_it() {
    let (mut s, clk, grid) = prefetched_to_three();
    let edge = 2.75 - 0.3 / 48_000.0;
    let _ = s.replace_pattern_continued_until(grid(), clk.now(), None, 2.6, edge, None);
    assert_eq!(s.scheduled_to_cycle(), 2.6);
    let begins = begins_through(&mut s, &clk, 3.1);
    assert_eq!(
        begins,
        vec![2.75, 3.0],
        "the onset on the takeover frame is the replacement's: {begins:?}"
    );
}

/// The other side of the edge: an onset a millionth of a frame before it is
/// the device's copy to play, and stays pre-marked.
#[test]
fn continued_replacement_until_an_edge_pre_marks_the_onset_just_before_it() {
    let (mut s, clk, grid) = prefetched_to_three();
    let edge = 2.75 + 1e-6 / 48_000.0;
    let _ = s.replace_pattern_continued_until(grid(), clk.now(), None, 2.6, edge, None);
    let begins = begins_through(&mut s, &clk, 3.1);
    assert_eq!(
        begins,
        vec![3.0],
        "the onset before the edge keeps its copy on the device: {begins:?}"
    );
}

/// The window reads onset times on the outgoing mapping, as the device holds
/// them. The onset at 2.75 is before the edge on that mapping and after the
/// edge on the replacement's mapping (0.1 ns later). It stays pre-marked.
#[test]
fn continued_replacement_until_an_edge_reads_the_outgoing_mapping() {
    for cps in [None, Some(CPS)] {
        let (mut s, clk, grid) = prefetched_to_three();
        clk.advance(1e-10);
        let from = s.cycle_at_time(clk.now());
        let edge = 2.75 + 1e-6 / 48_000.0;
        let _ = s.replace_pattern_continued_until(grid(), clk.now(), cps, from, edge, None);
        assert!(
            s.time_at_cycle(Fraction::new(11, 4)) >= edge,
            "test premise: the replacement's mapping puts the onset past the edge"
        );
        let begins = begins_through(&mut s, &clk, 3.1);
        assert_eq!(begins, vec![3.0], "{begins:?}");
    }
}

/// A replacement at another tempo reads the outgoing mapping too. A slower
/// one does not play again an onset the device keeps. A faster one plays an
/// onset the device drops, although its own time for it is before the edge.
#[test]
fn continued_replacement_until_an_edge_reads_the_outgoing_tempo() {
    let (mut s, clk, grid) = prefetched_to_three();
    let _ = s.replace_pattern_continued_until(grid(), clk.now(), Some(0.8 * CPS), 2.6, 2.9, None);
    assert!(s.time_at_cycle(Fraction::new(11, 4)) < 2.9 - 0.1);
    let mut begins = begins_through(&mut s, &clk, 3.1);
    clk.advance(0.45);
    begins.extend(begins_through(&mut s, &clk, 3.6));
    assert_eq!(begins, vec![3.0, 3.25], "{begins:?}");

    let (mut s, clk, grid) = prefetched_to_three();
    let _ = s.replace_pattern_continued_until(grid(), clk.now(), Some(1.25 * CPS), 2.6, 2.74, None);
    assert!(
        s.time_at_cycle(Fraction::new(11, 4)) < 2.74,
        "test premise: the replacement plays the onset before the edge"
    );
    let begins = begins_through(&mut s, &clk, 3.1);
    assert_eq!(begins, vec![2.75, 3.0], "{begins:?}");
}

/// An end cycle before the edge ends the window there: the replacement
/// plays the outgoing copies past it again.
#[test]
fn continued_replacement_until_an_edge_can_end_sooner() {
    let (mut s, clk, grid) = prefetched_to_three();
    let _ = s.replace_pattern_continued_until(grid(), clk.now(), None, 2.6, 2.9, Some(2.7));
    let begins = begins_through(&mut s, &clk, 3.1);
    assert_eq!(begins, vec![2.75, 3.0], "{begins:?}");

    // An end cycle past the edge changes nothing.
    let (mut s, clk, grid) = prefetched_to_three();
    let _ = s.replace_pattern_continued_until(grid(), clk.now(), None, 2.6, 2.9, Some(4.0));
    let begins = begins_through(&mut s, &clk, 3.1);
    assert_eq!(begins, vec![3.0], "{begins:?}");
}

/// The cover of the producer below: the horizon and a 0.25 s margin.
const COVER: f64 = HORIZON + 0.25;

/// A live device, as far as a takeover goes. It holds each event it gets.
/// A published takeover drops the events aimed at or after the edge.
#[derive(Default)]
struct Device {
    held: Vec<Event>,
}

impl Device {
    /// One producer turn at the clock: query, publish `takeover_edge` for a
    /// generation that is not on the device yet, then hand over the events.
    fn turn(&mut self, s: &mut Scheduler, clk: &VirtualClock, takeover_edge: Option<f64>) {
        assert_ne!(s.tick(clk), TickStatus::Refused);
        let events = s.drain_through(clk, clk.now() + COVER);
        if let Some(edge) = takeover_edge {
            self.held.retain(|event| event.target_time < edge);
        }
        self.held.extend(events);
    }

    /// A turn every 0.05 s, from step `from` to the step before `to`.
    fn play(&mut self, s: &mut Scheduler, clk: &VirtualClock, from: u32, to: u32) {
        for step in from..to {
            clk.set(f64::from(step) * 0.05);
            self.turn(s, clk, None);
        }
    }

    /// The times at which the onset at `begin` sounds.
    fn times_of(&self, begin: Fraction) -> Vec<f64> {
        self.held
            .iter()
            .filter(|event| event.whole_begin == begin)
            .map(|event| event.target_time)
            .collect()
    }

    /// Each onset of `steps` on the `1 / per_cycle` grid sounds once.
    fn assert_each_sounds_once(&self, per_cycle: i128, steps: std::ops::Range<i128>, case: &str) {
        for step in steps {
            let begin = Fraction::new(step, per_cycle);
            let times = self.times_of(begin);
            assert_eq!(
                times.len(),
                1,
                "{case}: the onset at {} sounded at {times:?}",
                begin.show()
            );
        }
    }
}

/// `per_cycle` onsets a cycle. Each value reads `slider` at query time, as
/// a hap under a slider does.
fn slid(per_cycle: usize, slider: &Arc<AtomicU64>) -> Pattern {
    let slider = Arc::clone(slider);
    fastcat(vec![atom("bd"); per_cycle])
        .fmap(move |_| Value::Str(format!("bd:{}", slider.load(Ordering::Relaxed))))
}

/// As [`slid`], with a number that goes up by one from onset to onset. One
/// step of the slider gives each hap the value its neighbour had.
fn stepped(per_cycle: usize, slider: &Arc<AtomicU64>) -> Pattern {
    let slider = Arc::clone(slider);
    fastcat((0..per_cycle).map(|step| atom(&step.to_string())).collect()).fmap(move |step| {
        let step: u64 = step.show().parse().expect("a step number");
        Value::Str(format!("n:{}", step + slider.load(Ordering::Relaxed)))
    })
}

/// `pattern` at one cycle a second on a device, played to clock 11.8.
fn playing_to_11_8(pattern: Pattern) -> (Scheduler, VirtualClock, Device) {
    let mut s = Scheduler::new(Arc::new(Transport::default()), CPS, COVER);
    let clk = VirtualClock::new(0.0);
    let mut device = Device::default();
    s.set_pattern(pattern, clk.now());
    device.play(&mut s, &clk, 0, 236);
    clk.set(11.8);
    (s, clk, device)
}

/// A save 0.1 s after a control requery. The device keeps the outgoing
/// onset at 12.0 across the two takeovers, so the save does not play it.
/// The slider can move, and the producer can miss its turn between the two.
#[test]
fn a_save_inside_the_margin_of_a_requery_sounds_each_onset_once() {
    for (moved, turn_between) in [(false, true), (true, true), (false, false), (true, false)] {
        let case = format!("slider moved {moved}, turn between {turn_between}");
        let slider = Arc::new(AtomicU64::new(1));
        let (mut s, clk, mut device) = playing_to_11_8(slid(8, &slider));
        if moved {
            slider.store(2, Ordering::Relaxed);
        }
        s.requery_active_from(11.8, 12.05).expect("a requery");
        if turn_between {
            device.turn(&mut s, &clk, Some(12.05));
            device.play(&mut s, &clk, 237, 238);
        }

        clk.set(11.9);
        let from = s.cycle_at_time(11.9);
        let _ = s.replace_pattern_continued_until(slid(8, &slider), 11.9, None, from, 12.15, None);
        // Without the turn between, the requery is never on the device.
        device.turn(&mut s, &clk, Some(12.15));
        device.play(&mut s, &clk, 239, 256);
        device.assert_each_sounds_once(8, 92..100, &case);
    }
}

/// A control requery in the takeover window of a save that changed the
/// tempo. The second takeover keeps the outgoing copies that the old tempo
/// aimed before its edge. The new generation plays only the others.
#[test]
fn a_requery_in_the_window_of_a_tempo_save_sounds_each_onset_once() {
    // The new tempo, then the clock and the takeover edge of the requery.
    // The save is at 11.9 and takes over at 12.15.
    for (cps, requery, edge) in [
        (0.6, 11.95, 12.2),
        (0.6, 11.93, 11.99),
        (2.0, 11.95, 12.2),
        (2.0, 11.93, 11.99),
    ] {
        for (moved, turn_between) in [(false, true), (true, true), (false, false), (true, false)] {
            let case = format!(
                "tempo {cps}, requery at {requery} to {edge}, slider moved {moved}, \
                 turn between {turn_between}"
            );
            let slider = Arc::new(AtomicU64::new(1));
            let (mut s, clk, mut device) = playing_to_11_8(stepped(32, &slider));
            device.play(&mut s, &clk, 237, 238);

            clk.set(11.9);
            let from = s.cycle_at_time(11.9);
            let save = stepped(32, &slider);
            let _ = s.replace_pattern_continued_until(save, 11.9, Some(cps), from, 12.15, None);
            if turn_between {
                device.turn(&mut s, &clk, Some(12.15));
            }

            clk.set(requery);
            if moved {
                slider.store(2, Ordering::Relaxed);
            }
            s.requery_active_from(requery, edge).expect("a requery");
            // Without the turn between, the save is never on the device.
            device.turn(&mut s, &clk, Some(edge));
            device.play(&mut s, &clk, 240, 256);
            device.assert_each_sounds_once(32, 380..400, &case);
        }
    }
}

/// A save 0.1 s after the requery of a clock steer. The steer moves the
/// mapping by a part of a frame, and the onset at 12.0 is still the
/// device's copy to play.
#[test]
fn a_save_inside_the_margin_of_a_clock_requery_sounds_each_onset_once() {
    let grid = || fastcat(vec![atom("bd"); 8]);
    let (mut s, clk, mut device) = playing_to_11_8(grid());
    s.retime(11.8, 1.000_001, s.cycle_at_time(11.8));
    s.requery_active_from(11.8, 12.05).expect("a requery");
    device.turn(&mut s, &clk, Some(12.05));
    device.play(&mut s, &clk, 237, 238);

    clk.set(11.9);
    let from = s.cycle_at_time(11.9);
    let _ = s.replace_pattern_continued_until(grid(), 11.9, None, from, 12.15, None);
    device.turn(&mut s, &clk, Some(12.15));
    device.play(&mut s, &clk, 239, 256);
    device.assert_each_sounds_once(8, 92..100, "a clock requery");
}

/// A clock bend keeps the cycle at `now`, so a cycle still names the same
/// onset. The onset at 193/16 is between the takeover edge on the old tempo
/// and on the new one. It sounds once, on a faster and on a slower bend.
#[test]
fn a_requery_after_a_clock_bend_sounds_each_onset_once() {
    for (cps, edge) in [(1.03, 12.06), (0.97, 12.07)] {
        let (mut s, clk, mut device) = playing_to_11_8(fastcat(vec![atom("bd"); 16]));
        s.retime(11.8, cps, s.cycle_at_time(11.8));
        s.requery_active_from(11.8, edge).expect("a requery");
        device.turn(&mut s, &clk, Some(edge));
        device.play(&mut s, &clk, 237, 256);
        device.assert_each_sounds_once(16, 190..200, &format!("a bend to {cps}"));
    }
}

/// 32 onsets a cycle of the sound `name`. A second control of each value
/// reads `slider` at query time.
fn lane(name: &'static str, slider: &Arc<AtomicU64>) -> Pattern {
    let slider = Arc::clone(slider);
    fastcat(vec![atom(name); 32])
        .fmap(move |_| Value::Str(format!("{name} x:{}", slider.load(Ordering::Relaxed))))
}

/// Each onset of `steps` on the 1/32 grid sounds once in each lane.
fn assert_each_lane_sounds_once(device: &Device, steps: std::ops::Range<i128>) {
    for step in steps {
        let begin = Fraction::new(step, 32);
        for name in ["bd", "hh"] {
            let times: Vec<f64> = device
                .held
                .iter()
                .filter(|event| event.whole_begin == begin && event.value.show().starts_with(name))
                .map(|event| event.target_time)
                .collect();
            assert_eq!(
                times.len(),
                1,
                "{name} at {} sounded at {times:?}",
                begin.show()
            );
        }
    }
}

/// The save changes each kick and adds a hat on its begin, and the outgoing
/// copy stands in for the kick. A requery in the window changes the two
/// again. The device holds one copy for the begin, so the hat is played.
#[test]
fn a_requery_in_the_window_keeps_one_mark_for_one_copy() {
    let slider = Arc::new(AtomicU64::new(1));
    let lane = |name| lane(name, &slider);
    let (mut s, clk, mut device) = playing_to_11_8(lane("bd"));
    device.play(&mut s, &clk, 237, 238);

    clk.set(11.9);
    slider.store(2, Ordering::Relaxed);
    let from = s.cycle_at_time(11.9);
    let save = stack(vec![lane("hh"), lane("bd")]);
    let _ = s.replace_pattern_continued_until(save, 11.9, Some(0.6), from, 12.15, None);
    device.turn(&mut s, &clk, Some(12.15));

    clk.set(11.95);
    slider.store(3, Ordering::Relaxed);
    s.requery_active_from(11.95, 12.2).expect("a requery");
    device.turn(&mut s, &clk, Some(12.2));
    device.play(&mut s, &clk, 240, 256);
    assert_each_lane_sounds_once(&device, 381..400);
}

/// Two copies sit on the begin 12.0: a kick aimed at 12.0 and a hat aimed at
/// 12.025. A second save changes both haps. A requery that takes over at 12.01
/// keeps the kick copy and drops the hat copy, so it plays the hat only.
#[test]
fn a_changed_hap_takes_the_time_of_the_copy_of_its_own_sound() {
    let slider = Arc::new(AtomicU64::new(1));
    let lanes = || stack(vec![lane("hh", &slider), lane("bd", &slider)]);
    let (mut s, clk, mut device) = playing_to_11_8(lane("bd", &slider));
    device.play(&mut s, &clk, 237, 238);

    clk.set(11.9);
    let from = s.cycle_at_time(11.9);
    let _ = s.replace_pattern_continued_until(lanes(), 11.9, Some(0.8), from, 12.15, None);
    device.turn(&mut s, &clk, Some(12.15));

    clk.set(11.95);
    slider.store(2, Ordering::Relaxed);
    let from = s.cycle_at_time(11.95);
    let _ = s.replace_pattern_continued_until(lanes(), 11.95, None, from, 12.2, None);
    device.turn(&mut s, &clk, Some(12.2));

    clk.set(11.98);
    s.requery_active_from(11.98, 12.01).expect("a requery");
    device.turn(&mut s, &clk, Some(12.01));
    device.play(&mut s, &clk, 240, 256);
    assert_each_lane_sounds_once(&device, 381..400);
}

/// The first pass of the requery ends between two copies the device keeps
/// past the cursor. The later copy still stands in for the changed hap on
/// its begin in the next pass.
#[test]
fn a_requery_in_the_window_holds_a_kept_copy_for_a_later_pass() {
    let slider = Arc::new(AtomicU64::new(1));
    let (mut s, clk, mut device) = playing_to_11_8(slid(32, &slider));
    device.play(&mut s, &clk, 237, 238);

    clk.set(11.9);
    let from = s.cycle_at_time(11.9);
    let _ =
        s.replace_pattern_continued_until(slid(32, &slider), 11.9, Some(0.6), from, 12.15, None);
    device.turn(&mut s, &clk, Some(12.15));

    clk.set(11.95);
    slider.store(2, Ordering::Relaxed);
    s.requery_active_from(11.95, 12.2).expect("a requery");
    // The cursor is at 12.08 and the kept copies at 12.09375 and 12.125.
    assert_eq!(s.tick_with_max_span_cycles(&clk, 0.03), TickStatus::Filled);
    assert!(s.scheduled_to_cycle() < 12.125, "test premise");
    device.turn(&mut s, &clk, Some(12.2));
    device.play(&mut s, &clk, 240, 256);
    device.assert_each_sounds_once(32, 380..400, "two passes");
}

/// A second save in the takeover window of a save that slowed the tempo.
/// The outgoing copies from 12.08 to 12.15 are before the second takeover
/// on the tempo they were aimed with, and after it on the new tempo.
#[test]
fn a_second_save_in_the_window_of_a_tempo_save_sounds_each_onset_once() {
    let grid = || fastcat(vec![atom("bd"); 32]);
    let (mut s, clk, mut device) = playing_to_11_8(grid());
    device.play(&mut s, &clk, 237, 238);

    clk.set(11.9);
    let from = s.cycle_at_time(11.9);
    let _ = s.replace_pattern_continued_until(grid(), 11.9, Some(0.6), from, 12.15, None);
    device.turn(&mut s, &clk, Some(12.15));

    clk.set(11.95);
    let from = s.cycle_at_time(11.95);
    let _ = s.replace_pattern_continued_until(grid(), 11.95, None, from, 12.2, None);
    device.turn(&mut s, &clk, Some(12.2));
    device.play(&mut s, &clk, 240, 256);
    device.assert_each_sounds_once(32, 380..400, "a second save");
}

/// A save to a slower tempo that the hap budget refuses never gets to the
/// device. The rollback 0.05 s later reads the copies of the generation
/// that still plays, at the times of its tempo.
#[test]
fn a_rollback_of_a_refused_tempo_save_sounds_each_onset_once() {
    let grid = || fastcat(vec![atom("bd"); 32]);
    let (mut s, clk, mut device) = playing_to_11_8(grid());
    device.play(&mut s, &clk, 237, 238);

    clk.set(11.9);
    let budget = s.query_hap_budget();
    s.set_query_hap_budget(1);
    let from = s.cycle_at_time(11.9);
    let _ = s.replace_pattern_continued_until(grid(), 11.9, Some(0.6), from, 12.15, None);
    assert_eq!(s.tick(&clk), TickStatus::Refused);

    clk.set(11.95);
    s.set_query_hap_budget(budget);
    let from = s.cycle_at_time(11.95);
    let _ = s.replace_pattern_continued_until(grid(), 11.95, Some(CPS), from, 12.2, None);
    device.turn(&mut s, &clk, Some(12.2));
    device.play(&mut s, &clk, 240, 256);
    device.assert_each_sounds_once(32, 380..400, "a rollback");
}

/// Two kicks a cycle. A save at 11.9 moves each onset 1/50 cycle earlier and
/// takes over at 12.15: the copy at 12.0 stands in for the hap at 11.98.
/// Returns the two patterns too.
fn moved_earlier_at_11_9() -> (Scheduler, VirtualClock, Device, [Pattern; 2]) {
    let grid = fastcat(vec![atom("bd"); 2]);
    let moved = grid.early(Fraction::new(1, 50));
    let (mut s, clk, mut device) = playing_to_11_8(grid.clone());
    device.play(&mut s, &clk, 237, 238);

    clk.set(11.9);
    let from = s.cycle_at_time(11.9);
    let _ = s.replace_pattern_continued_until(moved.clone(), 11.9, None, from, 12.15, None);
    device.turn(&mut s, &clk, Some(12.15));
    (s, clk, device, [grid, moved])
}

/// A second save at 11.99 puts the old pattern back. Its cursor is past the
/// hap at 11.98, and the device still holds the copy at 12.0 that stood in
/// for it, so the save does not play 12.0. A requery between changes nothing.
#[test]
fn a_second_save_after_a_stand_in_for_an_earlier_hap_sounds_each_onset_once() {
    // The cursor of the last requery is past the hap at 11.98 too.
    for requery in [None, Some(11.95), Some(11.985)] {
        let (mut s, clk, mut device, [grid, _]) = moved_earlier_at_11_9();
        if let Some(requery) = requery {
            clk.set(requery);
            s.requery_active_from(requery, requery + 0.25)
                .expect("a requery");
            device.turn(&mut s, &clk, Some(requery + 0.25));
        }

        clk.set(11.99);
        let from = s.cycle_at_time(11.99);
        let _ = s.replace_pattern_continued_until(grid, 11.99, None, from, 12.24, None);
        device.turn(&mut s, &clk, Some(12.24));
        device.play(&mut s, &clk, 240, 256);
        let case = format!("a requery between at {requery:?}");
        device.assert_each_sounds_once(2, 24..26, &case);
    }
}

/// The copy at 12.0 stands in only for a hap aimed at its own time. The
/// second save keeps the moved kicks and adds a kick at 12.06. The hap that
/// the copy is for is behind the cursor, so the added kick is played.
#[test]
fn a_copy_behind_the_cursor_leaves_an_added_onset_of_its_sound() {
    let (mut s, clk, mut device, [grid, moved]) = moved_earlier_at_11_9();

    clk.set(11.99);
    let from = s.cycle_at_time(11.99);
    let save = stack(vec![moved, grid.late(Fraction::new(3, 50))]);
    let _ = s.replace_pattern_continued_until(save, 11.99, None, from, 12.24, None);
    device.turn(&mut s, &clk, Some(12.24));
    device.play(&mut s, &clk, 240, 256);
    let added = device.times_of(Fraction::new(1206, 100));
    assert_eq!(added.len(), 1, "the added kick sounded at {added:?}");
    assert_eq!(device.times_of(Fraction::int(12)).len(), 1);
}

/// The same two saves for a device with no takeover frame: each save gives
/// its takeover as a cycle. The second save does not play 12.0 either.
#[test]
fn a_second_save_with_a_takeover_cycle_sounds_each_onset_once() {
    let grid = fastcat(vec![atom("bd"); 2]);
    let (mut s, clk, mut device) = playing_to_11_8(grid.clone());
    device.play(&mut s, &clk, 237, 238);

    clk.set(11.9);
    let from = s.cycle_at_time(11.9);
    let moved = grid.early(Fraction::new(1, 50));
    let _ = s.replace_pattern_continued(moved, 11.9, None, from, s.cycle_at_time(12.15));
    device.turn(&mut s, &clk, Some(12.15));

    clk.set(11.99);
    let from = s.cycle_at_time(11.99);
    let _ = s.replace_pattern_continued(grid, 11.99, None, from, s.cycle_at_time(12.24));
    device.turn(&mut s, &clk, Some(12.24));
    device.play(&mut s, &clk, 240, 256);
    device.assert_each_sounds_once(2, 24..26, "a takeover cycle");
}

/// The window of this save is 1.28 cycles and one query pass is one cycle.
/// Returns the begins the replacement emits before cycle 11.5. The device
/// holds the outgoing copies of the window, four to the cycle.
fn window_wider_than_a_pass(new: Pattern) -> Vec<f64> {
    let mut s = Scheduler::new(Arc::new(Transport::default()), CPS, 2.0);
    let clk = VirtualClock::new(0.0);
    s.set_pattern(four(atom("hh")), clk.now());
    for step in 0..=20 {
        clk.set(f64::from(step) * 0.5);
        assert_ne!(s.tick(&clk), TickStatus::Refused);
        let _ = s.drain_through(&clk, clk.now() + 2.0);
    }
    assert!(s.scheduled_to_cycle() >= 11.5, "test premise");

    let _ = s.replace_pattern_continued(new, 10.0, None, 10.0, 11.28);
    let mut begins = Vec::new();
    for _ in 0..2 {
        assert_eq!(s.tick_with_max_span_cycles(&clk, 1.0), TickStatus::Filled);
        begins.extend(
            s.drain_through(&clk, 12.0)
                .into_iter()
                .map(|event| event.whole_begin.to_f64())
                .filter(|begin| *begin < 11.5),
        );
    }
    begins.sort_by(f64::total_cmp);
    begins
}

/// A pass uses the outgoing copies of its own span only. The save doubles
/// the density. The copies at 11.0 and 11.25 wait for the second pass, so
/// the first pass emits each added hap of cycle 10.
#[test]
fn a_window_wider_than_a_pass_leaves_later_copies_to_the_later_pass() {
    let begins = window_wider_than_a_pass(fastcat(vec![atom("hh"); 8]));
    assert_eq!(
        begins,
        vec![10.125, 10.375, 10.625, 10.875, 11.125, 11.375],
        "the added haps, and from the takeover on every hap"
    );
}

/// A copy that a hap of an earlier pass matched is that hap's. The save
/// adds a lane of the same sound between the beats: the second pass does
/// not give the copies of cycle 10 to its added hap.
#[test]
fn a_window_wider_than_a_pass_spends_a_matched_copy() {
    let begins = window_wider_than_a_pass(stack(vec![
        four(atom("hh")),
        four(atom("hh")).late(Fraction::new(1, 8)),
    ]));
    assert_eq!(
        begins,
        vec![10.125, 10.375, 10.625, 10.875, 11.125, 11.375],
        "the added lane, and from the takeover on every hap"
    );
}

/// At `setcps(0.0005)` a millicycle is two seconds, so a 0.5 s horizon is a
/// deficit of 0.00025 cycles. A positive deficit over an unqueried span must
/// round up to one millicycle and query, never report a full horizon.
#[test]
fn a_sub_millicycle_deficit_queries_cycle_zero_rather_than_reporting_full() {
    let transport = Arc::new(Transport::default());
    // One cycle per 2000 seconds. Only `cps > 0` is required, so this is a
    // legal tempo; the 0.5 s horizon is 0.00025 cycles, under the
    // one-millicycle span quantum.
    let mut scheduler = Scheduler::new(transport, 0.0005, HORIZON);
    let clock = VirtualClock::new(0.0);
    scheduler.set_pattern(seq("bd", "sd"), clock.now());

    let mut profile = SchedulerTickProfile::default();
    assert_eq!(
        scheduler.tick_profiled(&clock, &mut profile),
        TickStatus::Filled,
        "a positive deficit with an unqueried horizon must query, not report HorizonFull"
    );
    assert!(!profile.horizon_full);
    assert_eq!(
        profile.query_span_millicycles, 1,
        "the sub-quantum deficit rounds up to exactly one millicycle"
    );
    assert_eq!(
        scheduler.scheduled_to_cycle(),
        0.001,
        "the query span covers cycle zero"
    );

    // The downbeat is scheduled: onset at cycle zero, target time at the
    // anchor. The `sd` half a cycle later is 1000 seconds away and stays
    // outside the one-millicycle window.
    let due = scheduler.drain_through(&clock, HORIZON);
    assert_eq!(
        due.iter()
            .map(|e| (e.whole_begin.show(), e.value.show(), e.target_time))
            .collect::<Vec<_>>(),
        [("0/1".into(), "bd".into(), 0.0)],
        "the cycle-zero downbeat must be queried and delivered"
    );

    // The live producer's gap skip now has nothing to drop: one millicycle at
    // this tempo is two seconds of cover, so the schedule stays ahead of the
    // clock and the downbeat can never be skipped as "unqueried past".
    clock.advance(HORIZON);
    assert_eq!(
        scheduler.skip_past_gap(clock.now(), HORIZON),
        None,
        "a covered clock must not gap-skip"
    );
}

/// A caller cap below the span quantum admits no representable span. The tick
/// reports `HorizonFull` and does not query past the cap.
#[test]
fn a_caller_cap_below_the_span_quantum_keeps_reporting_a_full_horizon() {
    let transport = Arc::new(Transport::default());
    let mut scheduler = Scheduler::new(transport, 0.0005, HORIZON);
    let clock = VirtualClock::new(0.0);
    scheduler.set_pattern(seq("bd", "sd"), clock.now());

    assert_eq!(
        scheduler.tick_with_max_span_cycles(&clock, 0.0005),
        TickStatus::HorizonFull,
        "a cap under one millicycle admits no representable span"
    );
    assert_eq!(
        scheduler.scheduled_to_cycle(),
        0.0,
        "a refused sub-quantum cap must not move the cursor past it"
    );
}

/// A horizon shorter than the smallest refill floor still queries once it is
/// drained, rather than reading as full over an empty schedule.
#[test]
fn a_sub_millisecond_horizon_queries_instead_of_reading_full_forever() {
    let clock = VirtualClock::new(0.0);
    let mut scheduler = Scheduler::new(Arc::new(Transport::default()), CPS, 0.0005);
    scheduler.set_pattern(atom("bd"), clock.now());
    assert_eq!(scheduler.horizon_remaining(clock.now()), 0.0);
    assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
    assert_eq!(scheduler.queued(), 1);
    // The one-millicycle round-up covers past `now + horizon`.
    assert_eq!(scheduler.tick(&clock), TickStatus::HorizonFull);
}

/// One cycle per 2000 s: a millicycle - the span quantum - is 2 s, four
/// times the horizon, so every fill rounds a sub-quantum deficit up to one.
const SLOW_CPS: f64 = 0.0005;
/// The tick grid both the offline loop and the live producer use for a
/// 0.5 s horizon (`refill_floor_secs`: a quarter of it, at most 50 ms).
const SLOW_STEP: f64 = 0.05;
/// `bd*5000` at `SLOW_CPS` is an onset every 0.4 s, five per millicycle, most
/// of them strictly inside a fill rather than on its boundary.
const SLOW_ONSETS_PER_CYCLE: i128 = 5000;
const SLOW_ONSET_SPACING: f64 = 0.4;
/// Four fills' worth of clock, with no onset exactly at the end or one
/// horizon past it.
const SLOW_RUN_TICKS: u32 = 156; // 7.8 s

fn slow_dense_scheduler() -> (Scheduler, VirtualClock) {
    let transport = Arc::new(Transport::default());
    let mut scheduler = Scheduler::new(transport, SLOW_CPS, HORIZON);
    let clock = VirtualClock::new(0.0);
    scheduler.set_pattern(
        atom("bd").fast(Fraction::int(SLOW_ONSETS_PER_CYCLE)),
        clock.now(),
    );
    (scheduler, clock)
}

/// Every onset `0, 0.4, 0.8, ...` whose target is at or before `until`, as
/// `(whole_begin, target_time)`.
fn slow_dense_onsets_through(until: f64) -> Vec<(Fraction, f64)> {
    (0..)
        .map(|k: i128| {
            (
                Fraction::new(k, SLOW_ONSETS_PER_CYCLE),
                k as f64 * SLOW_ONSET_SPACING,
            )
        })
        .take_while(|&(_, target)| target <= until)
        .collect()
}

/// Asserts `delivered` is exactly `expected`, in order, each at its exact
/// target time - so no onset is lost, doubled, or retimed.
fn assert_each_onset_once(delivered: &[(Fraction, f64, f64)], expected: &[(Fraction, f64)]) {
    assert_eq!(
        delivered
            .iter()
            .map(|&(begin, _, _)| begin.show())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|&(begin, _)| begin.show())
            .collect::<Vec<_>>(),
        "every onset must be delivered exactly once, in order"
    );
    for (&(begin, target, _), &(_, expected_target)) in delivered.iter().zip(expected) {
        assert!(
            (target - expected_target).abs() < 1e-9,
            "onset {} targets {target}, not {expected_target}",
            begin.show()
        );
    }
}

/// The widest gap between two ticks that actually queried.
fn widest_fill_gap(fills: &[f64]) -> f64 {
    fills.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max)
}

/// The offline `play` loop: tick, then `drain_due`. The round-up covers up to
/// 2 s past `now + horizon` at `SLOW_CPS`, longer than the 1 s stale limit.
/// Ticks over a covered horizon must keep the schedule fresh.
#[test]
fn a_slow_tempo_delivers_every_onset_across_fills_with_drain_due() {
    let (mut scheduler, clock) = slow_dense_scheduler();

    let mut fills = Vec::new();
    let mut delivered = Vec::new();
    for tick in 0..=SLOW_RUN_TICKS {
        clock.set(f64::from(tick) * SLOW_STEP);
        match scheduler.tick(&clock) {
            TickStatus::Filled => fills.push(clock.now()),
            TickStatus::HorizonFull => {}
            other => panic!("unexpected tick status {other:?} at {}", clock.now()),
        }
        for event in scheduler.drain_due(&clock) {
            delivered.push((event.whole_begin, event.target_time, clock.now()));
        }
    }

    // The run must actually hold a covered horizon past the stale limit, or
    // it proves nothing about the stale check.
    assert!(
        fills.len() >= 3 && widest_fill_gap(&fills) > 1.0,
        "the run never covered the horizon past the stale limit: fills at {fills:?}"
    );
    let end = f64::from(SLOW_RUN_TICKS) * SLOW_STEP;
    assert_each_onset_once(&delivered, &slow_dense_onsets_through(end));
    // `drain_due` hands an onset over at the first tick at or after its
    // target, never a fill later.
    for &(begin, target, at) in &delivered {
        assert!(
            target <= at + 1e-9 && at - target < SLOW_STEP + 1e-9,
            "onset {} (target {target}) was delivered at {at}",
            begin.show()
        );
    }
}

/// The same across the live producer's transfer: gap resync, tick, then
/// `drain_through` one horizon ahead of the clock. Every onset must reach the
/// consumer before its target, exactly once, and the covered schedule must
/// never look like a gap to skip. Treating the covered stretch as stale loses
/// the onsets that come due in it (3.2 s, 5.2 s and 7.2 s here); without the
/// round-up the deficit never reaches a millicycle, the resync keeps
/// skipping the unqueried cursor to the clock, and nothing is delivered.
#[test]
fn a_slow_tempo_delivers_every_onset_across_fills_with_drain_through() {
    let (mut scheduler, clock) = slow_dense_scheduler();

    let mut fills = Vec::new();
    let mut gap_skips = Vec::new();
    let mut delivered = Vec::new();
    for tick in 0..=SLOW_RUN_TICKS {
        clock.set(f64::from(tick) * SLOW_STEP);
        if let Some(dropped) = scheduler.skip_past_gap(clock.now(), HORIZON) {
            gap_skips.push((clock.now(), dropped));
        }
        match scheduler.tick(&clock) {
            TickStatus::Filled => fills.push(clock.now()),
            TickStatus::HorizonFull => {}
            other => panic!("unexpected tick status {other:?} at {}", clock.now()),
        }
        for event in scheduler.drain_through(&clock, clock.now() + HORIZON) {
            delivered.push((event.whole_begin, event.target_time, clock.now()));
        }
    }

    let end = f64::from(SLOW_RUN_TICKS) * SLOW_STEP;
    assert_each_onset_once(&delivered, &slow_dense_onsets_through(end + HORIZON));
    assert!(
        gap_skips.is_empty(),
        "a covered schedule was gap-skipped: {gap_skips:?}"
    );
    assert!(
        fills.len() >= 3 && widest_fill_gap(&fills) > 1.0,
        "the run never covered the horizon past the stale limit: fills at {fills:?}"
    );
    // Transferred ahead, never late, never past the lookahead deadline.
    for &(begin, target, at) in &delivered {
        assert!(
            target >= at - 1e-9 && target <= at + HORIZON + 1e-9,
            "onset {} (target {target}) was transferred at {at}",
            begin.show()
        );
    }
}

/// Only a covered horizon counts as fresh. A tick that cannot query (here a
/// caller cap under the span quantum) must not hold off stale expiry.
#[test]
fn a_tick_that_cannot_query_does_not_keep_a_schedule_fresh() {
    let (mut scheduler, clock) = slow_dense_scheduler();
    assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
    assert_eq!(
        scheduler
            .drain_due(&clock)
            .iter()
            .map(|e| e.whole_begin.show())
            .collect::<Vec<_>>(),
        ["0/1"]
    );

    // Past the covered horizon (it ends at 2 s, so 1.6 s leaves less than a
    // horizon of cover) and past the 1 s stale limit since the fill.
    clock.set(1.6);
    assert_eq!(
        scheduler.tick_with_max_span_cycles(&clock, 0.0005),
        TickStatus::HorizonFull,
        "a sub-quantum cap admits no span"
    );
    assert!(
        scheduler.drain_due(&clock).is_empty(),
        "an uncovered, unrefilled schedule must still expire as stale"
    );
}
