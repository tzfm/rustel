use rustel_audio::tripwire::{self, TripwireAlloc, Violations};
use rustel_csound::{Event, Instrument, Program};
use std::sync::Mutex;

#[global_allocator]
static ALLOCATOR: TripwireAlloc = TripwireAlloc;
static SERIAL: Mutex<()> = Mutex::new(());

fn program(source: &str) -> Option<Program> {
    match Program::compile(&[source.into()]) {
        Ok(program) => Some(program),
        Err(error)
            if std::env::var_os("RUSTEL_CSOUND_LIBRARY").is_none()
                && error.starts_with("Csound 6 is not available") =>
        {
            None
        }
        Err(error) => panic!("{error}"),
    }
}

fn event(instrument: Instrument, duration: f64) -> Event {
    Event {
        instrument,
        p4: 440.0,
        p5: 0.2,
        p6: "freq/440/gain/0.2/cutoff/900".into(),
        duration,
    }
}

#[test]
fn native_timing_cut_nested_notes_reset_and_callback_allocation() {
    let _serial = SERIAL.lock().unwrap();
    let Some(program) = program(
        "instr parent\nschedule \"constant\", 0, 1, 440, p5\n\
         schedule \"constant\", 0.002, 1, 440, p5\nturnoff\nendin\n\
         instr constant\naSignal init p5\nouts aSignal, aSignal\nendin",
    ) else {
        return;
    };
    assert_eq!(program.active_instances(), 0);
    let mut insert = program.instance(48_000).unwrap();
    assert_eq!(program.active_instances(), 1);
    let first = program
        .prepare(event(Instrument::Named("parent".into()), 1.0))
        .unwrap();
    let second = program
        .prepare(event(Instrument::Named("constant".into()), 1.0))
        .unwrap();
    let mut left = [0.0; 128];
    let mut right = [0.0; 128];
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        insert.score(first, 17);
        insert.cut_notes(65);
        insert.score(second, 91);
        insert.submit(&mut left, &mut right);
        insert.collect(&mut left, &mut right);
    });
    assert!(Violations::capture().since(before).clean());
    assert!(left[..17].iter().all(|v| *v == 0.0));
    assert!(left[18..65].iter().all(|v| (*v - 0.2).abs() < 1e-6));
    assert!(left[65..91].iter().all(|v| *v == 0.0));
    assert!(left[92..].iter().all(|v| (*v - 0.2).abs() < 1e-6));
    assert_eq!(left, right);
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        insert.reset();
        insert.reset();
        insert.process(&mut left, &mut right);
    });
    assert!(Violations::capture().since(before).clean());
    assert!(left.iter().all(|v| *v == 0.0));
    assert_eq!(program.pending_events(), 0);
    assert!(program.take_errors().is_empty());
    drop(insert);
    assert_eq!(program.active_instances(), 0);
}

#[test]
fn reset_suspends_globals_until_the_next_score() {
    let _serial = SERIAL.lock().unwrap();
    let Some(program) = program(
        "instr global\naSignal init 0.125\nouts aSignal, aSignal\nendin\n\
         alwayson \"global\"\ninstr trigger\nendin",
    ) else {
        return;
    };
    let mut insert = program.instance(48_000).unwrap();
    let mut left = [0.0; 128];
    let mut right = [0.0; 128];
    insert.process(&mut left, &mut right);
    assert!(left[1..].iter().all(|v| *v == 0.125));
    let pending = program
        .prepare(event(Instrument::Named("trigger".into()), 1.0))
        .unwrap();
    insert.score(pending, 512);
    for _ in 0..3 {
        insert.reset();
        insert.process(&mut left, &mut right);
        assert!(left.iter().chain(&right).all(|v| *v == 0.0));
    }
    for _ in 0..5 {
        insert.process(&mut left, &mut right);
        assert!(left.iter().chain(&right).all(|v| *v == 0.0));
    }
    let id = program
        .prepare(event(Instrument::Named("trigger".into()), 1.0))
        .unwrap();
    insert.score(id, 17);
    insert.process(&mut left, &mut right);
    assert!(left[..17].iter().all(|v| *v == 0.0));
    assert!(left[18..].iter().all(|v| *v == 0.125));
    assert!(program.take_errors().is_empty());
}

#[test]
fn bundled_instruments_and_custom_pfields_render_at_host_rates() {
    let _serial = SERIAL.lock().unwrap();
    let Some(program) = program(
        "instr 77\nprint p4\nprints \"custom instrument\\n\"\nSmap strget p6\niCutoff strtod keymap(\"cutoff\", Smap)\naSignal oscili p5 / 127, cpsmidinn(p4)\naSignal tone aSignal, iCutoff\nouts aSignal, aSignal\nendin",
    ) else {
        return;
    };
    for rate in [44_100, 48_000] {
        for name in ["triangle", "organ", "pad", "bow", "Meta", "77"] {
            let mut insert = program.instance(rate).unwrap();
            let mut note = event(Instrument::Named(name.into()), 0.2);
            if name == "Meta" {
                note.p6 = format!("s/triangle/{}", note.p6);
            } else if name == "77" {
                note.instrument = Instrument::Number(77.25);
                note.p4 = 69.0;
                note.p5 = 80.0;
            }
            insert.score(program.prepare(note).unwrap(), 0);
            let mut peak = 0.0_f32;
            for _ in 0..100 {
                let mut left = [0.0; 128];
                let mut right = [0.0; 128];
                insert.process(&mut left, &mut right);
                assert!(left.iter().chain(&right).all(|v| v.is_finite()));
                peak = left.iter().fold(peak, |peak, v| peak.max(v.abs()));
            }
            assert!(program.take_errors().is_empty());
            assert!(peak > 0.001, "{name} at {rate} Hz was silent");
        }
    }
    assert!(Program::compile(&["instr broken\nno_such_opcode 1\nendin".into()]).is_err());
}

#[test]
fn runtime_errors_reach_the_host_and_a_corrected_note_recovers() {
    let _serial = SERIAL.lock().unwrap();
    let Some(program) =
        program("instr broken\naSignal oscili p5, p4, 99999\nouts aSignal, aSignal\nendin")
    else {
        return;
    };
    for name in ["missing", "broken"] {
        let mut insert = program.instance(48_000).unwrap();
        let id = program
            .prepare(event(Instrument::Named(name.into()), 1.0))
            .unwrap();
        let mut left = [0.0; 128];
        let mut right = [0.0; 128];
        let before = Violations::capture();
        tripwire::audio_scope(|| {
            insert.score(id, 0);
            insert.process(&mut left, &mut right);
        });
        assert!(Violations::capture().since(before).clean());
        assert!(left.iter().chain(&right).all(|v| *v == 0.0));
        assert!(!program.take_errors().is_empty());
        for _ in 0..3 {
            insert.process(&mut left, &mut right);
            assert!(left.iter().chain(&right).all(|v| *v == 0.0));
        }
        let id = program
            .prepare(event(Instrument::Named("triangle".into()), 0.1))
            .unwrap();
        insert.score(id, 0);
        insert.process(&mut left, &mut right);
        assert!(left.iter().any(|v| v.abs() > 0.001));
        assert!(program.take_errors().is_empty());
    }
}
