/*
midi_input.rs - MIDI input end to end, with the bus as a fake device
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! `midin()` from the score down to a control value, without a MIDI device.
//!
//! Every score here writes `await midin(...)`, which is required, not optional:
//! strudel.cc returns a promise, so a score that omits the `await` breaks on
//! strudel.cc. Making it work here would have quietly stranded every score
//! written against this runtime, so the promise is reproduced - resolved
//! immediately, since there is nothing to wait for.
//!
//! The fake source is the input bus itself: a score names a selector, the test
//! writes into that port with exactly the scalar API the driver thread uses,
//! and then queries. This is the only seam that exercises `ref`, `range`,
//! `appLeft` and the scheduler's onset gate together - which is where the
//! subtle failures are.

use rustel_fraction::Fraction;
use rustel_runtime::Session;

fn control(session: &mut Session, key: &str) -> Vec<Option<f64>> {
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    haps.iter()
        .map(|hap| {
            hap.value
                .as_object()
                .and_then(|map| map.get(key))
                .and_then(rustel_core::Value::as_f64)
        })
        .collect()
}

#[test]
fn midin_resolves_without_a_device_and_a_control_reads_zero() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"const cc = await midin('nothing-here'); note("60").lpf(cc(74).range(200, 8000))"#,
        )
        .expect("a missing device must not fail evaluation");
    assert_eq!(
        control(&mut session, "cutoff"),
        vec![Some(200.0)],
        "an untouched control must read the low bound, not NaN and not silence"
    );
}

/// The discreteness guard. `cc()` must be a hap WITH a whole: the scheduler
/// drops anything without an onset, so building it as an analog signal makes
/// `note(cc(74)...)` silently produce no sound at all.
#[test]
fn a_control_pattern_is_discrete_where_a_signal_is_not() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const cc = await midin('x'); note(cc(74).range(40, 52))"#)
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert_eq!(haps.len(), 1);
    assert!(
        haps[0].whole.is_some(),
        "a control pattern must have a whole, or the scheduler drops it"
    );

    // The negative control: a signal genuinely has none.
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"note(sine.range(40, 52))"#)
        .expect("evaluate");
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert!(
        haps.iter().all(|hap| hap.whole.is_none()),
        "sine was expected to be analog; this test's premise has changed"
    );
}

/// The whole point: a knob turned on the wire reaches a filter in the score.
#[test]
fn a_knob_written_on_the_bus_reaches_the_cutoff() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"const cc = await midin('fake');
               note("c2 c3").s("sawtooth").lpf(cc(74).range(200, 8000))"#,
        )
        .expect("evaluate");
    let bus = session.midi_input_bus();
    let port = bus
        .find("fake")
        .expect("the score must have named the port");

    port.observe_control_change(1, 74, 127);
    assert_eq!(
        control(&mut session, "cutoff"),
        vec![Some(8000.0), Some(8000.0)],
        "a knob at full must reach the top of the range"
    );

    port.observe_control_change(1, 74, 0);
    assert_eq!(
        control(&mut session, "cutoff"),
        vec![Some(200.0), Some(200.0)]
    );
}

#[test]
fn a_channel_filter_selects_one_channel() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"const cc = await midin('fake');
               stack(note("60").lpf(cc(74, 1).range(0, 1000)),
                     note("62").lpf(cc(74, 2).range(0, 1000)))"#,
        )
        .expect("evaluate");
    let port = session.midi_input_bus().find("fake").expect("port");
    port.observe_control_change(1, 74, 127);
    let cutoffs = control(&mut session, "cutoff");
    assert_eq!(
        cutoffs,
        vec![Some(1000.0), Some(0.0)],
        "channel 1 must move and channel 2 must not"
    );
}

/// A controller nobody plugged in must not take the music with it.
#[test]
fn a_missing_device_never_silences_the_score() {
    let mut with = Session::new().expect("session");
    with.evaluate(
        r#"const cc = await midin('fake');
               note("c2 c3 e3 g3").lpf(cc(74).range(200, 8000))"#,
    )
    .expect("evaluate");
    let plain_count = {
        let mut plain = Session::new().expect("session");
        plain
            .evaluate(r#"note("c2 c3 e3 g3").lpf(1000)"#)
            .expect("evaluate");
        plain
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query")
            .len()
    };
    assert_eq!(
        with.query(Fraction::ZERO, Fraction::ONE)
            .expect("query")
            .len(),
        plain_count,
        "a missing MIDI device deleted haps"
    );
}

/// Watch mode re-evaluates the score into the SAME Session on every save. If
/// that re-interned the port, every save would snap every knob back to zero and
/// the open-port count would climb until the limit refused new ones.
#[test]
fn re_evaluating_the_score_keeps_the_knob_value() {
    let source = r#"const cc = await midin('fake'); note("60").lpf(cc(74).range(200, 8000))"#;
    let mut session = Session::new().expect("session");
    session.evaluate(source).expect("evaluate");
    let bus = session.midi_input_bus();
    bus.find("fake")
        .expect("port")
        .observe_control_change(1, 74, 127);
    assert_eq!(control(&mut session, "cutoff"), vec![Some(8000.0)]);

    session.evaluate(source).expect("re-evaluate");
    assert_eq!(
        control(&mut session, "cutoff"),
        vec![Some(8000.0)],
        "a save reset the knob"
    );
    assert_eq!(bus.len(), 1, "a save opened a second port");
}

/// A score may name at most a few devices, so a typo in a watched file cannot
/// spawn a reader thread per keystroke.
#[test]
fn naming_more_devices_than_the_limit_is_refused_rather_than_unbounded() {
    let mut session = Session::new().expect("session");
    let mut source = String::new();
    for index in 0..(rustel_core::midi_in::MAX_INPUT_PORTS + 2) {
        source.push_str(&format!("midin('device-{index}');\n"));
    }
    source.push_str(r#"note("60")"#);
    let refused = session.evaluate(&source).is_err();
    assert!(refused, "naming nine devices must be refused");
    assert_eq!(
        session.midi_input_bus().len(),
        0,
        "a refused score must not publish its partially staged inputs"
    );
    assert!(
        session.midi_input_bus().find("device-0").is_none(),
        "a provisional input escaped a refused score"
    );
}

/// Input names belong to the same last-good transaction as the graph. A
/// candidate that constructs successfully but throws in its first live query
/// must not replace the listener set that is still driving the sounding song.
#[test]
fn a_failed_live_probe_keeps_the_last_good_input_generation() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const cc = await midin('last-good'); note('c4').lpf(cc(74).range(200, 8000))"#)
        .expect("last-good score");
    let generation = session.generation();
    let bus = session.midi_input_bus();
    let last_good = bus.find("last-good").expect("last-good input");
    last_good.observe_control_change(1, 74, 127);

    let error = session
        .reload_at(
            r#"const cc = await midin('refused-candidate'); "a b".add("x")"#,
            false,
            0.25,
        )
        .expect_err("the query-time type error must reject the live replacement");
    assert!(error.to_string().contains("last-good score kept"));
    assert_eq!(session.generation(), generation);
    assert!(bus.find("refused-candidate").is_none());
    assert_eq!(bus.len(), 1);
    assert_eq!(
        control(&mut session, "cutoff"),
        vec![Some(8000.0)],
        "the refused candidate disturbed the last-good input value"
    );
}

/// Even if user code tries to name an input from a pattern callback, the
/// query-time hook must be refused before it can publish a port or mutate the
/// active generation.
#[test]
fn query_time_midin_cannot_bypass_the_score_effect_transaction() {
    let mut session = Session::new().expect("session");
    session.evaluate("note('c4')").expect("last-good score");
    let generation = session.generation();
    let error = session
        .reload_at(
            "pure('candidate').fmap(value => { midin('query-leak'); return value; })",
            false,
            0.25,
        )
        .expect_err("query-time midin must be refused");

    assert!(error.to_string().contains("host effect"));
    assert_eq!(session.generation(), generation);
    assert!(session.midi_input_bus().find("query-leak").is_none());
}

/// Provisional handles from rejected saves must be released. Otherwise eight
/// bad edits permanently consume the per-score capacity and a later valid
/// eight-controller score can never recover without restarting the process.
#[test]
fn rejected_saves_never_exhaust_the_next_valid_scores_input_capacity() {
    let mut session = Session::new().expect("session");
    session.evaluate("note('c4')").expect("last-good score");

    for attempt in 0..(rustel_core::midi_in::MAX_INPUT_PORTS + 2) {
        let mut source = String::new();
        for input in 0..rustel_core::midi_in::MAX_INPUT_PORTS {
            source.push_str(&format!("await midin('refused-{attempt}-{input}');\n"));
        }
        source.push_str(r#""a b".add("x")"#);
        session
            .reload_at(&source, false, 0.25)
            .expect_err("candidate probe must fail");
        assert_eq!(
            session.midi_input_bus().len(),
            0,
            "rejected save {attempt} published provisional inputs"
        );
    }

    let mut valid = String::new();
    for input in 0..rustel_core::midi_in::MAX_INPUT_PORTS {
        valid.push_str(&format!("await midin('valid-{input}');\n"));
    }
    valid.push_str("note('e4')");
    session
        .reload_at(&valid, false, 0.5)
        .expect("a valid full-capacity score must recover");
    assert_eq!(
        session.midi_input_bus().len(),
        rustel_core::midi_in::MAX_INPUT_PORTS
    );
}

/// The live host may remain on A while many individually valid replacements
/// are accepted but never reach their first device prefill. Publishing B..Z
/// must supersede the prior candidate rather than eventually failing after
/// QuickJS has already installed its wrapper.
#[test]
fn many_unpublished_candidates_cannot_break_the_next_valid_midi_score() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const cc = await midin('audible'); note('c4').lpf(cc(74).range(200, 8000))"#)
        .expect("audible score");
    let audible_generation = session.generation();
    let bus = session.midi_input_bus();
    bus.snapshot_for(audible_generation, audible_generation);

    // More than the removed unresolved-generation ceiling.
    for candidate in 0..128 {
        session
            .reload_at(
                &format!(
                    "const cc = await midin('candidate-{candidate}'); note('d4').lpf(cc(74).range(200, 8000))"
                ),
                false,
                0.25,
            )
            .expect("a valid candidate publication must remain infallible");
    }

    session
        .reload_at(
            r#"const cc = await midin('final'); note('e4').lpf(cc(74).range(200, 8000))"#,
            false,
            0.5,
        )
        .expect("the final valid score must still install");
    let final_port = bus
        .find("final")
        .expect("final wrapper lost its input handle");
    final_port.observe_control_change(1, 74, 127);
    assert_eq!(control(&mut session, "cutoff"), vec![Some(8000.0)]);

    let retained = bus.snapshot_for(audible_generation, session.generation());
    assert_eq!(
        retained.len(),
        2,
        "only audible A and the final candidate remain"
    );
    assert!(retained.iter().any(|port| port.selector == "audible"));
    assert!(retained.iter().any(|port| port.selector == "final"));
    assert!(bus.find_retained("candidate-127").is_none());
}

/// A scheduler-only control re-query changes the scheduler generation without
/// re-evaluating the score, so its existing input set must be republished under
/// the new id until the device acknowledges the cutover.
#[test]
fn control_requery_keeps_the_active_input_connected_through_cutover() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const cc = await midin('controller'); note(cc(74).range(40, 80))"#)
        .expect("score");
    let bus = session.midi_input_bus();
    let port = bus.find("controller").expect("controller");
    let before = session.generation();
    let initial = bus.snapshot_for(before, before);
    assert!(
        initial
            .iter()
            .any(|candidate| std::sync::Arc::ptr_eq(candidate, &port))
    );

    let (audible, candidate) = session
        .requery_active_at(0.25)
        .expect("requery")
        .expect("active transport must produce a replacement generation");
    assert_eq!(audible, before);
    assert!(candidate > audible);
    let through_cutover = bus.snapshot_for(audible, candidate);
    assert_eq!(through_cutover.len(), 1);
    assert!(std::sync::Arc::ptr_eq(&through_cutover[0], &port));
}

/// Explicit restart is a new performance epoch. A key placed in the prior
/// timeline must not become a fresh gate when cycle zero starts again.
#[test]
fn restarting_transport_invalidates_pre_restart_key_hits() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.25).s('tri')"#)
        .expect("score");
    let port = session.midi_input_bus().find("keyboard").expect("keyboard");
    port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
    let mut before = Vec::new();
    port.keys.select(0.0, 1.0, 0, Some((0, 1)), &mut before);
    assert_eq!(before.len(), 1, "test setup did not place the key");

    session.restart_transport_at(10.0);
    let mut after = Vec::new();
    port.keys.select(0.0, 1.0, 0, None, &mut after);
    assert!(after.is_empty(), "a pre-restart key survived the new epoch");
}

/// `rustel query` inspects arbitrary spans and must stay deterministic, so a
/// query that is not a real trigger places no notes and has no side effect.
#[test]
fn a_query_that_is_not_a_trigger_places_no_keys() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const kb = await midikeys('fake'); kb(0.25).s("tri")"#)
        .expect("evaluate");
    let port = session.midi_input_bus().find("fake").expect("port");
    port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);

    let first = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    let second = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert!(
        first.is_empty() && second.is_empty(),
        "an inspection query placed notes, so it is not reproducible"
    );
}

/// A note-on must reach the score as a playable hap with the note, velocity
/// and source channel strudel.cc exposes.
#[test]
fn a_played_note_becomes_a_hap_with_velocity_and_channel() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const kb = await midikeys('fake'); kb(0.25).s("tri")"#)
        .expect("evaluate");
    let port = session.midi_input_bus().find("fake").expect("port");
    port.observe_note_on(rustel_core::midi_in::now_nanos(), 3, 64, 100);
    // Placement is a trigger-query concern; drive the ring directly so this
    // test does not depend on the scheduler's control bag.
    let mut hits = Vec::new();
    port.keys.select(
        0.0,
        1.0,
        rustel_core::midi_in::now_nanos(),
        Some((0, 1)),
        &mut hits,
    );
    assert_eq!(hits.len(), 1);
    assert_eq!(
        (hits[0].note, hits[0].velocity, hits[0].channel),
        (64, 100, 3)
    );
}

/// `await` is required for portability, not timing. Upstream's `midin`
/// returns a promise, so a score without `await` throws on strudel.cc and
/// must throw here too.
#[test]
fn midin_requires_await_the_way_strudel_does() {
    let mut session = Session::new().expect("session");
    let refused = session.evaluate(r#"note("60").lpf(midin('fake')(74).range(200, 8000))"#);
    assert!(
        refused.is_err(),
        "calling midin() without await must fail, as it does on strudel.cc"
    );

    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const cc = await midin('fake'); note("60").lpf(cc(74).range(200, 8000))"#)
        .expect("the awaited form must work");

    // Same for midikeys.
    let mut session = Session::new().expect("session");
    assert!(
        session
            .evaluate(r#"midikeys('fake')(0.25).s("tri")"#)
            .is_err(),
        "calling midikeys() without await must fail too"
    );
}

/// A device name is a name in either quote: the transpiler does not read the
/// `midin` argument as mini-notation, so a name with spaces stays whole.
#[test]
fn a_device_name_reaches_the_port_in_either_quote() {
    for source in [
        r#"const cc = await midin('knob'); note("60").lpf(cc(74))"#,
        r#"const cc = await midin("knob"); note("60").lpf(cc(74))"#,
    ] {
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("a name is a name");
        assert!(
            session.midi_input_bus().find("knob").is_some(),
            "the port never opened for {source}"
        );
    }

    // The case mini-notation destroyed: a name with spaces in it, which
    // it cut into a sequence of words.
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const cc = await midin("Bass Station II"); note("60").lpf(cc(74))"#)
        .expect("a name with spaces survives");
    assert!(session.midi_input_bus().find("Bass Station II").is_some());
}

/// A port that really is a pattern is still refused, and still says how
/// to fix it: there is no name to recover from a rhythm.
#[test]
fn a_patterned_midi_port_says_how_to_fix_it() {
    let mut session = Session::new().expect("session");
    let Err(error) = session.evaluate(r#"$: note("c4").midi("a b".fast(2))"#) else {
        panic!("a patterned port must be refused, not silently wrong");
    };
    let message = error.to_string();
    assert!(
        message.contains("SINGLE quotes") && message.contains("rustel devices"),
        "the error must say how to fix it, got: {message}"
    );
}

/// A knob on an LFO's rate and depth. This is the shape a musician actually
/// wants for a filter sweep: the LFO does the sweeping continuously in the
/// audio engine, and the knobs shape how fast and how deep. Upstream supports
/// it by sampling every `lfo()` config value applicatively; this port used to
/// refuse any patterned config outright, which made the whole idiom unusable.
#[test]
fn a_knob_can_drive_an_lfo_rate_and_depth() {
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"const cc = await midin('fake');
               s("saw*4").lpf(1200).lfo({ r: cc(74).range(0.1, 20), da: cc(75).range(0, 3000) })"#,
        )
        .expect("a patterned lfo config must evaluate");
    let port = session.midi_input_bus().find("fake").expect("port");

    let rate_and_depth = |session: &mut Session| {
        let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
        let lfo = haps[0]
            .value
            .as_object()
            .and_then(|map| map.get("lfo"))
            .and_then(rustel_core::Value::as_object)
            .expect("an lfo entry")
            .get("0")
            .and_then(rustel_core::Value::as_object)
            .expect("modulator 0")
            .clone();
        (
            lfo.get("rate").and_then(rustel_core::Value::as_f64),
            lfo.get("depthabs").and_then(rustel_core::Value::as_f64),
        )
    };

    // Untouched knobs sit at the bottom of their ranges.
    assert_eq!(rate_and_depth(&mut session), (Some(0.1), Some(0.0)));

    port.observe_control_change(1, 74, 127);
    port.observe_control_change(1, 75, 127);
    let (rate, depth) = rate_and_depth(&mut session);
    assert_eq!(rate, Some(20.0), "the knob did not reach the LFO rate");
    assert_eq!(depth, Some(3000.0), "the knob did not reach the LFO depth");
}

/// The shape a musician actually writes, straight out of the docs: the
/// keyboard on its own line, the pattern on the next, and `keys()` called
/// with no note length at all.
#[test]
fn a_keyboard_opened_on_one_line_plays_on_the_next() {
    let mut session = Session::new().expect("session");
    session
        .evaluate("const keys = await midikeys('Bass Station II')\n$: keys().s(\"piano\")\n")
        .expect("the score the docs teach");
    let port = session
        .midi_input_bus()
        .find("Bass Station II")
        .expect("the score asked to listen to a named port");
    port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
    let mut keys = Vec::new();
    port.keys.select(0.0, 1.0, 0, Some((0, 1)), &mut keys);
    assert_eq!(keys.len(), 1, "a key played reaches the score's pattern");
}
