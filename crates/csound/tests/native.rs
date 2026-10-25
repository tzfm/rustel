use rustel_audio::OrbitInsert;
use rustel_audio::tripwire::{self, TripwireAlloc, Violations};
use rustel_csound::{Event, Instrument, Program};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[global_allocator]
static ALLOCATOR: TripwireAlloc = TripwireAlloc;
static SERIAL: Mutex<()> = Mutex::new(());

/// The native engine is one library, so the tests take turns. The guard of
/// a test that panicked still hands the turn on: a poisoned lock would
/// fail every test after the first fault.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The schedule lead the live pipeline gives a note: five 128-frame
/// buffers of device latency and two of buffer.
const LEAD: u32 = 640;
const BLOCK: usize = 128;

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

fn event(instrument: &str, duration: f64) -> Event {
    Event {
        instrument: Instrument::Named(instrument.into()),
        p4: 440.0,
        p5: 0.2,
        p6: "freq/440/gain/0.2/cutoff/900".into(),
        duration,
    }
}

/// A stereo recording of the insert, one channel per field.
type Recording = (Vec<f32>, Vec<f32>);

/// A stereo recording of the insert, one block per call, at the cadence of
/// the sample rate the way the callback asks. The worker keeps its runway
/// ahead of this pace with room to spare.
fn play(insert: &mut dyn OrbitInsert, blocks: usize) -> Recording {
    let mut left = Vec::with_capacity(blocks * BLOCK);
    let mut right = Vec::with_capacity(blocks * BLOCK);
    for _ in 0..blocks {
        let mut block = ([0.0; BLOCK], [0.0; BLOCK]);
        insert.process(&mut block.0, &mut block.1);
        left.extend_from_slice(&block.0);
        right.extend_from_slice(&block.1);
        std::thread::sleep(Duration::from_millis(2));
    }
    (left, right)
}

/// Plays until `heard` says the recording has what the test waits for. The
/// bound covers an orchestra compile on the worker.
fn play_until(insert: &mut dyn OrbitInsert, heard: &dyn Fn(&Recording) -> bool) -> Recording {
    let start = Instant::now();
    let mut recording = (Vec::new(), Vec::new());
    while !heard(&recording) && start.elapsed() < Duration::from_secs(5) {
        let mut block = ([0.0; BLOCK], [0.0; BLOCK]);
        insert.process(&mut block.0, &mut block.1);
        recording.0.extend_from_slice(&block.0);
        recording.1.extend_from_slice(&block.1);
        std::thread::sleep(Duration::from_millis(2));
    }
    recording
}

/// The diagnostics of the worker, less the starvation line. A busy machine
/// can starve the worker while an orchestra compiles: that is a gap of
/// silence, not an error of the score.
fn errors(program: &Program) -> Vec<String> {
    let mut errors = program.take_errors();
    errors.retain(|line| line != rustel_csound::STARVATION_LINE);
    errors
}

#[test]
fn native_timing_cut_nested_notes_reset_and_callback_allocation() {
    let _serial = serial();
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
    // A cold worker fills its ring over the first blocks. That warm-up
    // silence stays out of the counts and the windows below.
    const WARM: usize = 3;
    let mut recording = play(insert.as_mut(), WARM);
    assert!(recording.0.iter().chain(&recording.1).all(|v| *v == 0.0));
    let starved_before = program.starved_frames();
    let first = program.prepare(event("parent", 1.0)).unwrap();
    let second = program.prepare(event("constant", 1.0)).unwrap();
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        // The note arrives with the lead the live pipeline gives it. Its
        // onset lands on its own frame, not on when the worker wakes.
        insert.score(first, LEAD + 17);
    });
    assert!(Violations::capture().since(before).clean());
    let played = play(insert.as_mut(), 8);
    recording.0.extend(&played.0);
    recording.1.extend(&played.1);
    let onset = WARM * BLOCK + LEAD as usize + 17;
    // The parent schedules a second nested note 96 frames in, and the two
    // constants sound together until the cut.
    let nested = onset + 96;
    assert!(recording.0[..onset].iter().all(|v| *v == 0.0));
    assert!(
        recording.0[onset + 1..nested]
            .iter()
            .all(|v| (v - 0.2).abs() < 1e-6)
    );
    assert!(recording.0[nested..].iter().all(|v| (v - 0.4).abs() < 1e-6));
    assert_eq!(recording.0, recording.1);
    // The worker held the whole window ready: nothing ran dry.
    assert_eq!(program.starved_frames(), starved_before);
    // A cut while the worker is ahead of the reader. The frames the old
    // orchestra rendered past the cut stay silent.
    let cut = (WARM + 8) * BLOCK + 64;
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        insert.cut_notes(64);
    });
    assert!(Violations::capture().since(before).clean());
    let more = play(insert.as_mut(), 2);
    recording.0.extend(&more.0);
    recording.1.extend(&more.1);
    assert!(
        recording.0[nested..cut]
            .iter()
            .all(|v| (v - 0.4).abs() < 1e-6)
    );
    assert!(recording.0[cut..].iter().all(|v| *v == 0.0));
    // A note after the restart: the orchestra compiles on the worker, in
    // the rest the cut left, and the note sounds once that is done.
    insert.score(second, LEAD + 91);
    let recovered = play_until(insert.as_mut(), &|recording| {
        recording.0.iter().any(|v| (v - 0.2).abs() < 1e-6)
    });
    assert!(recovered.0.iter().any(|v| (v - 0.2).abs() < 1e-6));
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        insert.reset();
        insert.reset();
    });
    assert!(Violations::capture().since(before).clean());
    let tail = play(insert.as_mut(), 2);
    assert!(tail.0.iter().chain(&tail.1).all(|v| *v == 0.0));
    assert_eq!(program.pending_events(), 0);
    // No error and no starvation line: the warm-up and the compile after
    // the cut read dry by design.
    assert!(program.take_errors().is_empty());
    drop(insert);
    assert_eq!(program.active_instances(), 0);
}

#[test]
fn a_cut_keeps_earlier_notes_and_notes_submitted_after_it() {
    let _serial = serial();
    let Some(program) = program("instr constant\naSignal init p5\nouts aSignal, aSignal\nendin")
    else {
        return;
    };
    let mut insert = program.instance(48_000).unwrap();
    let mut earlier = event("constant", 1.0);
    earlier.p5 = 0.1;
    let earlier = program.prepare(earlier).unwrap();
    let ghost = program.prepare(event("constant", 1.0)).unwrap();
    let mut kept = event("constant", 1.0);
    kept.p5 = 0.3;
    let kept = program.prepare(kept).unwrap();
    let mut restarted = event("constant", 1.0);
    restarted.p5 = 0.4;
    let restarted = program.prepare(restarted).unwrap();
    insert.score(earlier, 0);
    insert.score(ghost, 512);
    insert.cut_notes(320);
    insert.score(kept, 320);
    insert.cut_notes(900);
    insert.score(restarted, 900);
    insert.prime(1024);
    let (left, right) = play(insert.as_mut(), 8);
    assert!(left[..320].iter().all(|v| (v - 0.1).abs() < 1e-6));
    assert!(left[320..900].iter().all(|v| (v - 0.3).abs() < 1e-6));
    assert!(left[900..].iter().all(|v| (v - 0.4).abs() < 1e-6));
    assert_eq!(left, right);
    let next = program.prepare(event("constant", 1.0)).unwrap();
    insert.score(next, LEAD + 17);
    let live = play(insert.as_mut(), 8);
    let onset = LEAD as usize + 17;
    assert!(live.0[..onset].iter().all(|v| (v - 0.4).abs() < 1e-6));
    assert!(live.0[onset..].iter().all(|v| (v - 0.6).abs() < 1e-6));
    assert_eq!(live.0, live.1);
    assert_eq!(program.pending_events(), 0);
    assert!(errors(&program).is_empty());
}

#[test]
fn reset_suspends_globals_until_the_next_score() {
    let _serial = serial();
    let Some(program) = program(
        "instr global\naSignal init 0.125\nouts aSignal, aSignal\nendin\n\
         alwayson \"global\"\ninstr trigger\nendin",
    ) else {
        return;
    };
    let mut insert = program.instance(48_000).unwrap();
    // A cold worker fills its ring while the first blocks play, so the
    // global settles in over a short warm-up instead of the first frame.
    let warm = play_until(insert.as_mut(), &|recording| {
        recording.0.iter().any(|v| (v - 0.125).abs() < 1e-6)
    });
    assert!(warm.0.iter().any(|v| (v - 0.125).abs() < 1e-6));
    let steady = play(insert.as_mut(), 1);
    assert!(steady.0.iter().all(|v| (v - 0.125).abs() < 1e-6));
    let pending = program.prepare(event("trigger", 1.0)).unwrap();
    insert.score(pending, 512);
    for _ in 0..3 {
        insert.reset();
        let block = play(insert.as_mut(), 1);
        assert!(block.0.iter().chain(&block.1).all(|v| *v == 0.0));
    }
    for _ in 0..5 {
        let block = play(insert.as_mut(), 1);
        assert!(block.0.iter().chain(&block.1).all(|v| *v == 0.0));
    }
    // Those blocks carried the reader past the restart, so this reset
    // arms a fresh mask over an orchestra already gone. It has no cut to
    // confirm it: without confirming itself, the mask would outlive it
    // and the global below would never come back.
    insert.reset();
    let block = play(insert.as_mut(), 1);
    assert!(block.0.iter().chain(&block.1).all(|v| *v == 0.0));
    let id = program.prepare(event("trigger", 1.0)).unwrap();
    insert.score(id, 17);
    // The orchestra compiles on the worker, and the global comes back with
    // the note that asked for it.
    let back = play_until(insert.as_mut(), &|recording| {
        recording.0.iter().any(|v| (v - 0.125).abs() < 1e-6)
    });
    assert!(back.0.iter().any(|v| (v - 0.125).abs() < 1e-6));
    let held = play(insert.as_mut(), 2);
    assert!(held.0[128..].iter().all(|v| (v - 0.125).abs() < 1e-6));
    assert!(errors(&program).is_empty());
}

#[test]
fn bundled_instruments_and_custom_pfields_render_at_host_rates() {
    let _serial = serial();
    let Some(program) = program(
        "instr 77\nprint p4\nprints \"custom instrument\\n\"\nSmap strget p6\niCutoff strtod keymap(\"cutoff\", Smap)\naSignal oscili p5 / 127, cpsmidinn(p4)\naSignal tone aSignal, iCutoff\nouts aSignal, aSignal\nendin",
    ) else {
        return;
    };
    for rate in [44_100, 48_000] {
        for name in ["triangle", "organ", "pad", "bow", "Meta", "77"] {
            let mut insert = program.instance(rate).unwrap();
            let mut note = event(name, 0.2);
            if name == "Meta" {
                note.p6 = format!("s/triangle/{}", note.p6);
            } else if name == "77" {
                note.instrument = Instrument::Number(77.25);
                note.p4 = 69.0;
                note.p5 = 80.0;
            }
            insert.score(program.prepare(note).unwrap(), 0);
            // The worker renders the frames before the test reads them: a
            // slow machine compiles the orchestra after the first reads.
            insert.prime(16 * BLOCK);
            let (left, right) = play(insert.as_mut(), 16);
            assert!(left.iter().chain(&right).all(|v| v.is_finite()));
            let peak = left.iter().fold(0.0_f32, |peak, v| peak.max(v.abs()));
            assert!(peak > 0.001, "{name} at {rate} Hz was silent");
            assert!(errors(&program).is_empty());
        }
    }
    assert!(Program::compile(&["instr broken\nno_such_opcode 1\nendin".into()]).is_err());
}

#[test]
fn runtime_errors_reach_the_host_and_a_corrected_note_recovers() {
    let _serial = serial();
    let Some(program) =
        program("instr broken\naSignal oscili p5, p4, 99999\nouts aSignal, aSignal\nendin")
    else {
        return;
    };
    for name in ["missing", "broken"] {
        let mut insert = program.instance(48_000).unwrap();
        let id = program.prepare(event(name, 1.0)).unwrap();
        let mut left = [0.0; BLOCK];
        let mut right = [0.0; BLOCK];
        let before = Violations::capture();
        tripwire::audio_scope(|| {
            insert.score(id, 0);
            insert.process(&mut left, &mut right);
        });
        assert!(Violations::capture().since(before).clean());
        let silent = play(insert.as_mut(), 3);
        assert!(silent.0.iter().chain(&silent.1).all(|v| *v == 0.0));
        assert!(!errors(&program).is_empty());
        let id = program.prepare(event("triangle", 0.1)).unwrap();
        insert.score(id, 0);
        let corrected = play_until(insert.as_mut(), &|recording| {
            recording.0.iter().any(|v| v.abs() > 0.001)
        });
        assert!(corrected.0.iter().any(|v| v.abs() > 0.001));
        assert!(errors(&program).is_empty());
    }
}
