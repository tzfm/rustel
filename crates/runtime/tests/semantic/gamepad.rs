/*
gamepad.rs - gamepad() from the score down to a value, without a pad
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! `gamepad()` end to end: the score names a pad, the test presses its
//! buttons and moves its sticks with exactly the API the poller thread
//! uses, and then queries. No device is opened - the poller starts on
//! the first `gamepad()` and finds none - so every reading starts from
//! nothing pressed, which is what a page with no pad reads too.
//!
//! The tests share pad 0 and run one at a time on it.

use std::sync::Mutex;

use rustel_fraction::Fraction;
use rustel_runtime::Session;

static PAD_0: Mutex<()> = Mutex::new(());

fn pad() -> (
    &'static rustel_core::gamepad::Pad,
    std::sync::MutexGuard<'static, ()>,
) {
    let guard = PAD_0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pad = rustel_core::gamepad::pad(0).expect("pad 0");
    pad.set_connected(false);
    (pad, guard)
}

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

fn onsets(session: &mut Session) -> usize {
    session
        .query(Fraction::ZERO, Fraction::ONE)
        .expect("query")
        .iter()
        .filter(|hap| hap.whole.is_some())
        .count()
}

#[test]
fn a_button_masks_the_notes_while_it_is_held() {
    let (pad, _guard) = pad();
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const gp = gamepad(0); note("c a f e").mask(gp.a)"#)
        .expect("no pad is not an error");
    assert_eq!(onsets(&mut session), 0, "nothing plays with the button up");
    pad.set_button(0, 1.0);
    assert_eq!(onsets(&mut session), 4, "held, every note plays");
    pad.set_button(0, 0.0);
    assert_eq!(
        onsets(&mut session),
        0,
        "released, they stop - read live, not at evaluation"
    );
}

#[test]
fn a_toggle_flips_on_every_press_and_the_names_come_in_both_cases() {
    let (pad, _guard) = pad();
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const gp = gamepad(); s("bd*2").mask(gp.tglLB).bank("RolandTR909")"#)
        .expect("evaluate");
    let toggled_before = pad.toggle(4);
    let now = |session: &mut Session| onsets(session);
    let before = now(&mut session);
    pad.set_button(4, 1.0);
    pad.set_button(4, 0.0);
    let after = now(&mut session);
    assert_ne!(before, after, "one press flips it");
    assert_ne!(pad.toggle(4), toggled_before);
    pad.set_button(4, 1.0);
    pad.set_button(4, 0.0);
    assert_eq!(now(&mut session), before, "and the next flips it back");

    let mut session = Session::new().expect("session");
    for name in [
        "A", "lb", "LB", "tglA", "tglLb", "tglUp", "tglU", "tglL3", "tglLs", "tglStart", "tglBACK",
        "u", "RIGHT",
    ] {
        session
            .evaluate(&format!(
                r#"const gp = gamepad(); note("c").mask(gp.{name})"#
            ))
            .unwrap_or_else(|error| panic!("gp.{name}: {error}"));
    }
}

#[test]
fn sticks_read_zero_to_one_and_minus_one_to_one() {
    let (pad, _guard) = pad();
    let mut session = Session::new().expect("session");
    session
        .evaluate(r#"const gp = gamepad(0); note("60").lpf(gp.x1.range(100, 4000)).lpq(gp.y1_2.range(0, 10))"#)
        .expect("evaluate");
    assert_eq!(
        control(&mut session, "cutoff"),
        vec![Some(2050.0)],
        "a centred stick is half way: 0.5 of the range"
    );
    assert_eq!(
        control(&mut session, "resonance"),
        vec![Some(0.0)],
        "-1 to 1, centred at 0: range() maps it as it is, so the bipolar reading is for range2()"
    );
    pad.set_axis(0, -1.0);
    pad.set_axis(1, 1.0);
    assert_eq!(
        control(&mut session, "cutoff"),
        vec![Some(100.0)],
        "hard left is the bottom"
    );
    assert_eq!(
        control(&mut session, "resonance"),
        vec![Some(10.0)],
        "pushed down is the top"
    );
    pad.set_axis(1, -1.0);
    assert_eq!(
        control(&mut session, "resonance"),
        vec![Some(-10.0)],
        "pushed up is past the bottom"
    );
    let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
    assert!(
        haps.iter().all(|hap| hap.whole.is_some()),
        "a reading is discrete, like a cc: the scheduler would drop a signal"
    );
}

#[test]
fn a_button_sequence_is_one_for_two_seconds_after_it_lands() {
    let (pad, _guard) = pad();
    let mut session = Session::new().expect("session");
    session
        .evaluate(
            r#"const gp = gamepad(0)
const HADOUKEN = ['d', 'r', 'a']
$: s("bd").mask(gp.btnSequence(HADOUKEN))
$: s("sd").mask(gp.btnSeq('dra'))"#,
        )
        .expect("evaluate");
    assert_eq!(onsets(&mut session), 0);
    for button in [13usize, 15, 0] {
        pad.set_button(button, 1.0);
        pad.set_button(button, 0.0);
    }
    assert_eq!(onsets(&mut session), 2, "both spellings saw the combo");
    pad.set_button(1, 1.0);
    pad.set_button(1, 0.0);
    assert_eq!(onsets(&mut session), 0, "a press after it ends it");

    let mut session = Session::new().expect("session");
    let error = session
        .evaluate(r#"const gp = gamepad(); s("bd").mask(gp.btnSequence('dq'))"#)
        .expect_err("q is no button");
    assert!(
        error.to_string().contains("no button is called 'q'"),
        "{error}"
    );
    let error = session
        .evaluate(r#"gamepad(4)"#)
        .expect_err("four pads at most");
    assert!(error.to_string().contains("at most 4 pads"), "{error}");
}
