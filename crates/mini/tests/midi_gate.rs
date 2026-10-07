/// Patterned note length is sampled at the key press time. Playback starts at
/// the scheduler frontier, which can fall on the other side of a gate change.
/// Using that later time can select the wrong note length.
#[test]
fn a_patterned_length_is_read_where_the_key_was_struck() {
    use rustel_core::{Value, midi_in};
    use rustel_fraction::Fraction;
    use std::sync::Arc;

    let lengths = rustel_mini::mini("0.01 1").expect("lengths");
    let port = Arc::new(midi_in::InputPort::new("probe".into()));
    let pattern = rustel_core::midi_keys(lengths, Arc::clone(&port));

    // The key press is a hundredth of a cycle before the half, on the short
    // side. The query starts a hundredth of a cycle after the half, because
    // the scheduler frontier is always a little later than the press.
    let cps = 0.5;
    let late_by_cycles = 0.02;
    // The press comes first and the query comes later, as in live playback.
    port.observe_note_on(midi_in::now_nanos(), 1, 60, 100);
    std::thread::sleep(std::time::Duration::from_secs_f64(late_by_cycles / cps));

    let frontier = Fraction::from_f64(0.5 + 0.01).expect("frontier");
    let end = Fraction::from_f64(0.75).expect("end");
    let mut state = rustel_core::State::new(rustel_core::TimeSpan::new(frontier, end));
    state.controls.insert("_cps".into(), Value::F64(cps));
    let haps = pattern.query(&state);
    assert_eq!(haps.len(), 1, "one press, one note: {haps:?}");
    let whole = haps[0].whole.expect("a struck note has a whole");
    let length = whole.end.sub(whole.begin).to_f64();
    assert!(
        length < 0.5,
        "the short side of the gate the finger was on, got {length}"
    );

    // And it says where in the text that gate lives, so the editor can
    // light the element the finger landed on. Without it a player watches
    // a pattern they cannot see and has to guess which half they are on.
    assert!(
        !haps[0].context.is_empty(),
        "the note carries the length's source span: {:?}",
        haps[0].context
    );
}

/// Later queries preserve the key's original gate position. Stack, jux and
/// live takeover use the lookback fixed when the key was placed.
#[test]
fn a_re_query_keeps_the_length_the_finger_chose() {
    use rustel_core::{Value, midi_in};
    use rustel_fraction::Fraction;
    use std::sync::Arc;
    use std::time::Duration;

    let lengths = rustel_mini::mini("0.01 1").expect("lengths");
    let port = Arc::new(midi_in::InputPort::new("probe".into()));
    let pattern = rustel_core::midi_keys(lengths, Arc::clone(&port));

    // On the long side, a tenth of a cycle after the gate, with the
    // stamp already a few milliseconds old so the first query does not
    // have to sleep.
    let cps = 1.0;
    let stamp = midi_in::now_nanos().saturating_sub(5_000_000);
    port.observe_note_on(stamp, 1, 60, 100);

    let frontier = Fraction::from_f64(0.7).expect("frontier");
    let end = Fraction::from_f64(1.0).expect("end");
    let mut state = rustel_core::State::new(rustel_core::TimeSpan::new(frontier, end));
    state.controls.insert("_cps".into(), Value::F64(cps));
    let first = pattern.query(&state);
    assert_eq!(first.len(), 1, "one press, one note: {first:?}");
    let first_whole = first[0].whole.expect("a struck note has a whole");
    let first_length = first_whole.end.sub(first_whole.begin).to_f64();
    assert!(
        first_length > 0.5,
        "the long side of the gate the finger was on, got {first_length}"
    );

    // Long enough that a recomputed lookback would cross back onto the
    // short side (0.2 cycles at 1 cps), and short enough that wrapping
    // around the previous cycle cannot save it.
    std::thread::sleep(Duration::from_millis(200));
    let second = pattern.query(&state);
    assert_eq!(second.len(), 1, "the press is still there: {second:?}");
    let second_whole = second[0].whole.expect("a struck note has a whole");
    let second_length = second_whole.end.sub(second_whole.begin).to_f64();
    assert!(
        second_length > 0.5,
        "a later re-query walked the finger back across the gate, got {second_length}"
    );
}
