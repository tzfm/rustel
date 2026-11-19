//! A hap's `cps` control changes the tempo at its onset.
//!
//! Each onset sounds at the tempo in force at it, no onset is skipped or
//! doubled when the tempo rises or falls inside one fill, and an outside
//! clock that owns the tempo turns the control off.

use rustel_core::{Pattern, Value, fastcat, pure, slowcat, stack};
use rustel_fraction::Fraction;
use rustel_scheduler::{Clock, Event, Scheduler, Transport, VirtualClock};
use std::sync::Arc;

fn note() -> Pattern {
    pure(Value::Str("n".into()))
}

/// Four notes a cycle.
fn quarters() -> Pattern {
    fastcat(vec![note(), note(), note(), note()])
}

fn tempo(cps: f64) -> Pattern {
    pure(Value::object([("cps".into(), Value::F64(cps))]))
}

/// Play `pattern` from time zero for `seconds`, ticking every 10 ms, and
/// give each event delivered with the horizon each tick left.
fn play(scheduler: &mut Scheduler, pattern: Pattern, seconds: f64) -> Vec<Event> {
    let clock = VirtualClock::new(0.0);
    scheduler.set_pattern(pattern, clock.now());
    let mut events = Vec::new();
    while clock.now() < seconds {
        scheduler.tick(&clock);
        events.extend(scheduler.drain_through(&clock, clock.now() + 1.0));
        clock.advance(0.01);
    }
    events
}

fn notes(events: &[Event]) -> Vec<(Fraction, f64, Option<f64>)> {
    events
        .iter()
        .filter(|event| event.value == Value::Str("n".into()))
        .map(|event| (event.whole_begin, event.target_time, event.cps))
        .collect()
}

fn scheduler(cps: f64, horizon: f64) -> Scheduler {
    Scheduler::new(Arc::new(Transport::default()), cps, horizon)
}

/// Each note at quarter `q` of the first `cycles` cycles, at the time
/// `time_of(q)`, exactly once.
fn assert_notes(
    played: &[(Fraction, f64, Option<f64>)],
    cycles: i128,
    time_of: impl Fn(i128) -> f64,
) {
    for q in 0..cycles * 4 {
        let begin = Fraction::new(q, 4);
        let at: Vec<_> = played.iter().filter(|(b, ..)| *b == begin).collect();
        assert_eq!(at.len(), 1, "quarter {q}: {at:?}");
        let want = time_of(q);
        assert!(
            (at[0].1 - want).abs() < 1e-9,
            "quarter {q} at {} s, want {want} s",
            at[0].1
        );
    }
}

/// The tempo rises from 1 to 2 at cycle 1 and falls back at cycle 2. The
/// notes of the faster cycle all play, half as far apart, and the note on
/// the cycle of a change already plays at the new tempo.
#[test]
fn a_rising_tempo_skips_no_onset() {
    let mut scheduler = scheduler(1.0, 0.5);
    let pattern = stack(vec![quarters(), slowcat(vec![tempo(1.0), tempo(2.0)])]);
    let played = notes(&play(&mut scheduler, pattern, 5.0));
    // Cycle c of a pair starts at 1.5 s per pair: 1 s at cps 1, 0.5 s at 2.
    assert_notes(&played, 4, |q| {
        let (pair, rest) = (q / 8, q % 8);
        let start = pair as f64 * 1.5;
        if rest < 4 {
            start + rest as f64 * 0.25
        } else {
            start + 1.0 + (rest - 4) as f64 * 0.125
        }
    });
    let cps_of = |q: i128| {
        played
            .iter()
            .find(|(begin, ..)| *begin == Fraction::new(q, 4))
            .map(|(.., cps)| *cps)
            .expect("the note")
    };
    assert_eq!(cps_of(0), None, "cycle 0 plays at the session tempo");
    assert_eq!(cps_of(4), Some(2.0), "the change comes first on its cycle");
    assert_eq!(cps_of(8), Some(1.0));
}

/// The tempo falls from 4 to 1/4 at cycle 1. The fill that finds the change
/// stops at the horizon, and later fills play on from there.
#[test]
fn a_falling_tempo_stops_the_fill_at_the_horizon() {
    let mut scheduler = scheduler(4.0, 0.5);
    let pattern = stack(vec![
        quarters(),
        slowcat(vec![tempo(4.0), tempo(0.25), tempo(0.25), tempo(0.25)]),
    ]);
    let clock = VirtualClock::new(0.0);
    scheduler.set_pattern(pattern, clock.now());
    let mut events = Vec::new();
    while clock.now() < 9.0 {
        scheduler.tick(&clock);
        let queued = scheduler.drain_through(&clock, f64::INFINITY);
        for event in &queued {
            assert!(
                event.target_time <= clock.now() + 0.5 + 1e-9,
                "an onset at {} s is past the horizon at {} s",
                event.target_time,
                clock.now()
            );
        }
        events.extend(queued);
        clock.advance(0.01);
    }
    let played = notes(&events);
    // Cycle 0 takes 1/4 s; each later quarter takes 1 s.
    assert_notes(&played, 3, |q| {
        if q < 4 {
            q as f64 / 16.0
        } else {
            0.25 + (q - 4) as f64
        }
    });
}

/// The clock reaches each change once, and the tempo that plays follows.
#[test]
fn the_tempo_that_plays_follows_the_clock() {
    let mut scheduler = scheduler(1.0, 0.5);
    let pattern = stack(vec![quarters(), slowcat(vec![tempo(1.0), tempo(2.0)])]);
    let clock = VirtualClock::new(0.0);
    scheduler.set_pattern(pattern, clock.now());
    let mut changes = Vec::new();
    let mut playing = scheduler.cps();
    while clock.now() < 2.0 {
        scheduler.tick(&clock);
        let _ = scheduler.drain_due(&clock);
        if scheduler.cps() != playing {
            playing = scheduler.cps();
            changes.push((clock.now(), playing));
        }
        clock.advance(0.01);
    }
    let times: Vec<f64> = changes.iter().map(|(time, _)| *time).collect();
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!((times[0] - 1.0).abs() < 0.011 && changes[0].1 == 2.0);
    assert!((times[1] - 1.5).abs() < 0.011 && changes[1].1 == 1.0);
}

/// A tempo hap longer than a fill is in several fills, and only the one
/// with its onset changes the tempo.
#[test]
fn a_tempo_hap_without_its_onset_changes_nothing() {
    let mut scheduler = scheduler(1.0, 0.5);
    let pattern = stack(vec![quarters(), tempo(2.0).slow(Fraction::int(4))]);
    let played = notes(&play(&mut scheduler, pattern, 2.0));
    assert_notes(&played, 3, |q| q as f64 * 0.125);
}

/// An outside clock that owns the tempo turns the control off.
#[test]
fn an_outside_clock_ignores_the_control() {
    let mut scheduler = scheduler(1.0, 0.5);
    scheduler.set_pattern_tempo(false);
    let pattern = stack(vec![quarters(), slowcat(vec![tempo(1.0), tempo(2.0)])]);
    let played = notes(&play(&mut scheduler, pattern, 3.0));
    assert_notes(&played, 2, |q| q as f64 * 0.25);
    assert!(played.iter().all(|(.., cps)| cps.is_none()));
    assert_eq!(scheduler.cps(), 1.0);
}

/// The time of `end` ahead under `scheduler`, querying `pattern` cycle by
/// cycle as a tick would.
fn ahead(scheduler: &Scheduler, pattern: &Pattern, end: Fraction) -> f64 {
    scheduler.time_at_cycle_ahead(end, |state| match pattern.query_arc_outcome(state) {
        Ok(rustel_core::QueryArcOutcome::Haps(haps)) => Some(haps),
        _ => None,
    })
}

/// Before a fill meets them, the time of a cycle ahead counts each tempo
/// change the pattern makes on the way, and matches where the played
/// notes land.
#[test]
fn the_time_of_a_cycle_ahead_counts_the_tempo_changes_on_the_way() {
    let pattern = stack(vec![
        quarters(),
        slowcat(vec![tempo(1.0), tempo(2.0), tempo(1.0), tempo(2.0)]),
    ]);
    let session_tempo = scheduler(0.5, 0.5);
    // One cycle at 1 cps, one at 2, one at 1, one at 2.
    assert_eq!(ahead(&session_tempo, &pattern, Fraction::int(4)), 3.0);
    assert_eq!(ahead(&session_tempo, &pattern, Fraction::new(3, 2)), 1.25);

    let played = notes(&play(&mut scheduler(0.5, 0.5), pattern.clone(), 4.0));
    let downbeat = |cycle: i128| {
        played
            .iter()
            .find(|(begin, ..)| *begin == Fraction::int(cycle))
            .map(|(_, time, _)| *time)
    };
    assert_eq!(downbeat(3), Some(2.5));
    assert_eq!(
        ahead(&session_tempo, &pattern, Fraction::int(3)),
        2.5,
        "the time ahead is where the notes play"
    );

    // Without a tempo control, a cycle lasts as the session tempo says.
    assert_eq!(ahead(&session_tempo, &quarters(), Fraction::int(4)), 8.0);
    // An outside clock that owns the tempo leaves the pattern's tempo out.
    let mut owned = scheduler(0.5, 0.5);
    owned.set_pattern_tempo(false);
    assert_eq!(ahead(&owned, &pattern, Fraction::int(4)), 8.0);
}
