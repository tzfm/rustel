//! The native query deadline reports itself as a query deadline.
//!
//! A pattern can spend its time in native recursion with no JavaScript, such
//! as `fill` under `struct` under `fill`. The refusal names the query and the
//! milliseconds it was allowed. It does not name a JavaScript CPU deadline.

use std::time::{Duration, Instant};

use rustel_core::{QueryLimit, State, TimeSpan, Value, fastcat, pure, with_query_deadline};
use rustel_fraction::Fraction;

/// Wide enough that the deadline, sampled every sixty-fourth node, is seen.
fn wide() -> rustel_core::Pattern {
    fastcat((0..4096).map(|i| pure(Value::F64(f64::from(i)))).collect())
}

#[test]
fn a_native_deadline_is_refused_as_a_query_deadline() {
    let state = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
    let refused = with_query_deadline(Instant::now(), || wide().query_arc_outcome(&state));
    match refused {
        Err(QueryLimit::QueryDeadline { millis }) => {
            assert_eq!(millis, 0, "a deadline already due allowed nothing");
            assert_eq!(
                QueryLimit::QueryDeadline { millis }.to_string(),
                "the query exceeded its 0 ms deadline and was refused"
            );
        }
        other => panic!("expected the native deadline refusal, got {other:?}"),
    }
}

#[test]
fn the_allowance_is_measured_from_the_installation() {
    let allowed = with_query_deadline(Instant::now() + Duration::from_millis(400), || {
        rustel_core::installed_query_deadline_millis()
    });
    assert!(
        matches!(allowed, Some(millis) if (300..=400).contains(&millis)),
        "{allowed:?}"
    );
    assert_eq!(
        rustel_core::installed_query_deadline_millis(),
        None,
        "nothing is installed outside the scope"
    );
}

#[test]
fn a_nested_deadline_never_reports_more_than_the_outer_one_allowed() {
    let inner = with_query_deadline(Instant::now() + Duration::from_millis(50), || {
        with_query_deadline(Instant::now() + Duration::from_secs(2), || {
            rustel_core::installed_query_deadline_millis()
        })
    });
    assert!(matches!(inner, Some(millis) if millis <= 50), "{inner:?}");
}
