/*
rustel-runtime - scheduling callback-bearing graphs
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Callback-bearing graphs must work through `play` and `render`, not only
//! direct queries. The scheduler queries graphs on its own clock, outside
//! `JsRuntime::query`, and therefore installs the callback host for each tick.
//!
//! Every case here is a source whose graph is classified IMPURE, so it must
//! reach the callback host during scheduling. The pure cases are kept alongside
//! them deliberately: they must keep scheduling with NO host installed, which
//! exercises the purity invariant on this path.

use rustel_fraction::Fraction;
use rustel_runtime::{RenderFormat, Session};

/// Sources whose graphs need the JavaScript host at query time.
///
/// Both `every` arguments are patterned in the first two, so the transformer
/// cannot be applied eagerly. The scheduler's own query must resolve it.
const HOST_REQUIRED_SOURCES: &[&str] = &[
    r#"note("c e g").every(fastcat(2, 3), x => x.fast(2))"#,
    r#"s("bd sd").every(fastcat(2, 3), x => x.fast(2))"#,
    // `pure(x)`, not `x.fast(2)`: the hap VALUE is a control object, and
    // calling `.fast` on one is a `TypeError` that empties the query.
    r#"s("bd").polyBind(x => pure(x).fast(2))"#,
    r#"s("bd sd").stepBind(x => fastcat(x, x))"#,
    r#"s("bd sd").sometimesBy("<0.3 0.6>", x => x.speed(2))"#,
    r#"note("[c,e,g]").arpWith(haps => fastcat(haps[2], haps[0]))"#,
    r#"wchooseCycles([note("[c,e,g]").arpWith(haps => haps[0]), pure(1).arpWith(haps => haps[0])], [note("d"), 0])"#,
    r#"note("c d").pickF(0, pure([x => x.fast(2)]))"#,
    // A bare JS-owned value must be materialised while the host still exists,
    // before the scheduler retains it for a later drain/render phase.
    r#"pure(['bd']).fast(2)"#,
];

/// ...and sources that reach none, which must schedule with no host at all.
const PURE_SOURCES: &[&str] = &[r#"s("bd sd")"#, r#"s("bd sd").fast(2)"#, r#"note("c e g")"#];

#[test]
fn play_schedules_host_required_graphs_without_panicking() {
    for source in HOST_REQUIRED_SOURCES {
        let mut session = Session::new().expect("session");
        session
            .evaluate(source)
            .unwrap_or_else(|e| panic!("{source}: evaluate failed: {e}"));

        let report = session
            .play(2.0)
            .unwrap_or_else(|e| panic!("{source}: play failed: {e}"));
        assert!(
            !report.onsets.is_empty(),
            "{source}: scheduled nothing. An empty timeline is how a swallowed \
             callback failure looks - the query error empties the whole arc."
        );
        assert_eq!(report.device_audio, "not-requested");
    }
}

#[test]
fn render_schedules_host_required_graphs_without_panicking() {
    let dir = std::env::temp_dir().join("rustel-scheduling-test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    for (i, source) in HOST_REQUIRED_SOURCES.iter().enumerate() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(source)
            .unwrap_or_else(|e| panic!("{source}: evaluate failed: {e}"));
        let out = dir.join(format!("render-{i}.json"));
        session
            .render(1.0, &out, RenderFormat::OnsetJson)
            .unwrap_or_else(|e| panic!("{source}: render failed: {e}"));
        assert!(out.exists(), "{source}: render wrote nothing");
        let _ = std::fs::remove_file(&out);
    }
}

#[test]
fn scheduled_onsets_agree_with_the_queried_haps() {
    // Not just "it did not panic": the timeline has to carry the same onsets
    // the query does, with the same VALUES and the same multiplicity.
    //
    // Match complete (position, value) multisets so wrong values, dropped
    // duplicates, and extra onsets all fail.
    for source in HOST_REQUIRED_SOURCES.iter().chain(PURE_SOURCES) {
        let mut session = Session::new().expect("session");
        session
            .evaluate(source)
            .unwrap_or_else(|e| panic!("{source}: evaluate failed: {e}"));

        // One cycle at the default cps of 0.5 is exactly two seconds of wall
        // clock, so the play window and the query window are the same span.
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .unwrap_or_else(|e| panic!("{source}: query failed: {e}"));
        let onsets = session
            .play(2.0)
            .unwrap_or_else(|e| panic!("{source}: play failed: {e}"))
            .onsets;

        // Only ONSETS are scheduled, and an onset is a hap whose PART begins
        // where its WHOLE does. A hap with no whole is analog; a fragment whose
        // part starts later is a continuation of an event already triggered.
        //
        // Getting that wrong is what made an earlier draft of this test fail on
        // `s("bd").polyBind(x => pure(x).fast(2))`: pinned Node returns two
        // haps there, `(0/1 → 1/2) ⇝ 1/1` and `0/1 ⇜ (1/2 → 1/1)`, which share
        // ONE whole of `0/1 → 1/1`. Two onsets would have been two triggers of
        // the same note.
        let mut want: Vec<(String, String)> = haps
            .iter()
            .filter_map(|h| h.whole.map(|w| (w, h)))
            .filter(|(w, h)| w.begin == h.part.begin)
            .filter(|(w, _)| w.begin >= Fraction::ZERO && w.begin < Fraction::ONE)
            .map(|(w, h)| (w.begin.show(), h.value.show()))
            .collect();
        want.sort();

        let mut got: Vec<(String, String)> = onsets
            .iter()
            .filter(|o| o.target_time < 2.0)
            .map(|o| (o.whole_begin.clone(), o.value_show.clone()))
            .collect();
        got.sort();

        assert!(
            !want.is_empty(),
            "{source}: the query itself produced no onsets, so this comparison \
             proves nothing"
        );
        assert_eq!(
            got, want,
            "{source}: the scheduled timeline disagrees with the query"
        );
    }
}

#[test]
fn pure_graphs_still_schedule_with_no_callback_host() {
    // The complement, and the reason the fix checks `active_needs_host()`
    // rather than always installing a scope: a graph classified PURE must never
    // need JavaScript. If one of these started requiring the host, the
    // classification would be wrong and the purity guarantee hollow.
    for source in PURE_SOURCES {
        let mut session = Session::new().expect("session");
        session
            .evaluate(source)
            .unwrap_or_else(|e| panic!("{source}: evaluate failed: {e}"));
        assert!(
            !session.active_needs_host(),
            "{source} reaches no JavaScript and must classify pure"
        );
        let report = session
            .play(2.0)
            .unwrap_or_else(|e| panic!("{source}: play failed: {e}"));
        assert!(!report.onsets.is_empty(), "{source}: scheduled nothing");
    }
}

/// Heap headroom above what the session already holds; the callback requests
/// much more. A callback that exhausts the QuickJS heap must fail scheduling
/// with a typed refusal, not a panic and not `Ok([])`.
const HEAP_HEADROOM: usize = 8 * 1024 * 1024;
const HEAP_HOG: &str = r#"note("c*16").every(fastcat(1, 1), x => { globalThis.__hog = new Array(4000000).fill(7); return x; })"#;

fn hogging_session() -> Session {
    let mut session = Session::new().expect("session");
    session
        .evaluate(HEAP_HOG)
        .expect("the pattern itself must evaluate");
    session
        .set_js_memory_limit(session.js_heap_live() + HEAP_HEADROOM)
        .expect("lowering the ceiling to just above current usage");
    session
}

#[test]
fn a_callback_heap_exhaustion_is_a_typed_refusal_through_query() {
    let session = hogging_session();
    let err = session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect_err("an exhausted heap must not come back as an empty query");
    assert_eq!(
        err.kind(),
        "resource-limit",
        "a heap refusal must be typed as a resource limit, got {err}"
    );
}

#[test]
fn a_callback_heap_exhaustion_is_a_typed_refusal_through_play_and_render() {
    for (what, result) in [
        ("play", hogging_session().play(0.1).map(|_| ())),
        (
            "render",
            hogging_session()
                .render(
                    0.1,
                    &std::env::temp_dir()
                        .join(format!("rustel-heap-render-{}.json", std::process::id())),
                    RenderFormat::OnsetJson,
                )
                .map(|_| ()),
        ),
    ] {
        let err = result.expect_err(&format!(
            "{what}: an exhausted heap must be a refusal, not a successful empty timeline"
        ));
        assert_eq!(
            err.kind(),
            "resource-limit",
            "{what}: a heap refusal must be typed as a resource limit, got {err}"
        );
        assert!(
            err.to_string().contains("JavaScript heap"),
            "{what}: the specific HostMemory refusal was changed or erased: {err}"
        );
    }
}

/// Staged external intents survive a failed live step in the Session, and
/// only the host's `take_pending_*` removes them. That is the session-side
/// half of the live-tick discipline the CLI and the studio engine both
/// keep: take every external family at the TOP of the tick and drop what a
/// failed step's tick took. A host that took them only after a successful
/// step would send them on a LATER tick. (Here the intents were staged by
/// the successful first window; the failed step leaves them in place. The
/// CLI's own half is `take_tick_intents`, tested in the binary.)
#[test]
#[cfg(all(feature = "device-audio", feature = "serial", feature = "osc"))]
fn a_failed_live_step_leaves_staged_external_intents_for_its_host_to_drop() {
    use std::cell::Cell;
    use std::time::Duration;

    use rustel_runtime::LiveFileProducer;

    // The callback uses much heap on every query and routes every onset to
    // every external family. The first window stages the intents. With the
    // ceiling lowered, the next window's query is refused, and the staged
    // intents stay for the host to find.
    const HOGGING_EXTERNAL_SOURCE: &str = r#"note("c*4").every(fastcat(1, 1), x => { globalThis.__hog = new Array(4000000).fill(7); return x.serial(9600).osc(57121).midi("test-port"); })"#;

    let mut session = Session::new().expect("session");
    session
        .evaluate(HOGGING_EXTERNAL_SOURCE)
        .expect("the score itself must evaluate");
    let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
    let clock = Cell::new(0.0f64);
    producer
        .step_unwatched_with_clock(&mut session, || clock.get(), 48_000, |_| true)
        .expect("the first window schedules under the default heap ceiling");
    session
        .set_js_memory_limit(session.js_heap_live() + HEAP_HEADROOM)
        .expect("lowering the ceiling to just above the retained hog");

    // Move past the arc the first window covered so the step must query a
    // fresh cycle: its callback reallocates the hog, which no longer fits.
    clock.set(4.0);
    let err = producer
        .step_unwatched_with_clock(&mut session, || clock.get(), 48_000, |_| true)
        .expect_err("the hogged query must refuse under the tightened ceiling");
    assert_eq!(
        err.kind(),
        "resource-limit",
        "a heap refusal must be typed as a resource limit, got {err}"
    );

    // The failed step does not clear what is staged: every family survives
    // in the Session until the host takes it. A host that took them only
    // after a successful step would send these on a later tick.
    #[cfg(feature = "midi")]
    assert!(
        !session.take_pending_midi().is_empty(),
        "the routed MIDI intents must survive the failed step for its host to drop"
    );
    assert!(
        !session.take_pending_serial().is_empty(),
        "the routed serial intents must survive the failed step for its host to drop"
    );
    assert!(
        !session.take_pending_osc().is_empty(),
        "the routed OSC intents must survive the failed step for its host to drop"
    );
    // And one take is a complete drain: the tick that takes at the top and
    // drops what a failed step took leaves nothing for the next tick.
    #[cfg(feature = "midi")]
    assert!(
        session.take_pending_midi().is_empty(),
        "one take must drain every staged MIDI intent"
    );
    assert!(
        session.take_pending_serial().is_empty(),
        "one take must drain every staged serial intent"
    );
    assert!(
        session.take_pending_osc().is_empty(),
        "one take must drain every staged OSC intent"
    );
}
