use rustel_core::tonaljs::{
    interval_semitones, note_get, note_transpose, scale_step, scale_tokenize,
};
use rustel_core::{QueryArcOutcome, State, TimeSpan, Value, combinators, pure};
use rustel_fraction::Fraction;

fn query_first_cycle(pattern: &rustel_core::Pattern) -> QueryArcOutcome {
    let state = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
    pattern.query_arc_outcome(&state).expect("ordinary query")
}

#[test]
fn extreme_user_interval_is_rejected() {
    // A valid i64 literal but an invalid signed magnitude for pitch algebra.
    assert_eq!(interval_semitones("-9223372036854775808P"), None);
    assert_eq!(note_transpose("C4", "-9223372036854775808P"), "");
}

#[test]
fn unicode_prefix_on_shorthand_interval_is_safe() {
    // Shorthand parsing searches for a quality later in the string. The
    // search must advance by UTF-8 characters, not bytes within "é".
    assert_eq!(interval_semitones("éP5"), Some(7));
    assert_eq!(interval_semitones("éwat"), None);
}

#[test]
fn extreme_note_octave_is_rejected() {
    assert!(note_get("C9223372036854775807").is_none());
    assert_eq!(note_transpose("C9223372036854775807", "2M"), "");
}

#[test]
fn extreme_scale_degree_does_not_panic() {
    // Numeric scale degrees are directly supplied by a score's `.scale` call.
    assert!(scale_step(f64::MAX, "C:major").is_err());
}

#[test]
fn scale_type_starts_after_the_original_tonic_spelling() {
    assert_eq!(scale_tokenize("Cx émajor"), ("C##".into(), "émajor".into()));
    assert_eq!(scale_tokenize("Cx major"), ("C##".into(), "major".into()));
    assert_eq!(scale_tokenize("c03 major"), ("C3".into(), "major".into()));
}

#[test]
fn score_scale_with_unicode_type_does_not_abort_query() {
    let scaled = combinators::scale(
        &pure(Value::Str("0".into())),
        Value::Str("Cx:émajor".into()),
    );
    let QueryArcOutcome::Haps(haps) = query_first_cycle(&scaled) else {
        panic!("invalid scale name must not abort the query");
    };
    assert!(haps.is_empty());
}

#[test]
fn score_combinators_survive_extreme_tonal_inputs() {
    let transposed = combinators::transpose(
        &pure(Value::Str("C4".into())),
        Value::Str("-9223372036854775808P".into()),
    );
    let QueryArcOutcome::Haps(haps) = query_first_cycle(&transposed) else {
        panic!("invalid interval must not abort the query");
    };
    assert_eq!(haps.len(), 1);
    assert_eq!(haps[0].value, Value::Str(String::new()));

    let prefixed = combinators::transpose(&pure(Value::Str("C4".into())), Value::Str("éP5".into()));
    let QueryArcOutcome::Haps(haps) = query_first_cycle(&prefixed) else {
        panic!("Unicode prefix must not abort the query");
    };
    assert_eq!(haps[0].value, Value::Str("G4".into()));

    let scaled = combinators::scale(&pure(Value::F64(f64::MAX)), Value::Str("C:major".into()));
    let QueryArcOutcome::Haps(haps) = query_first_cycle(&scaled) else {
        panic!("out-of-range scale degree must not abort the query");
    };
    assert!(haps.is_empty());
}
