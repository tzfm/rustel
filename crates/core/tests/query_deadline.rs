//! The native query deadline reports itself as a query deadline.
//!
//! A pattern can spend its time in native recursion with no JavaScript, such
//! as `fill` under `struct` under `fill`. The refusal names the query and the
//! milliseconds it was allowed. It does not name a JavaScript CPU deadline.

use std::time::{Duration, Instant};

use rustel_core::{
    QueryLimit, State, TimeSpan, Value, fastcat, pure, with_query_budget, with_query_deadline,
};
use rustel_fraction::Fraction;

/// Wide enough that the deadline, sampled every sixty-fourth node, is seen.
fn wide() -> rustel_core::Pattern {
    of_steps(4096)
}

/// Wide enough for two samples: one starts a budget, and the next ends it.
fn wider() -> rustel_core::Pattern {
    of_steps(32_768)
}

fn of_steps(count: i32) -> rustel_core::Pattern {
    fastcat((0..count).map(|i| pure(Value::F64(f64::from(i)))).collect())
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

#[test]
fn a_budget_bounds_a_query_from_its_first_read_of_the_clock() {
    let state = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
    // No time: the first sample starts the budget, and the next one ends it.
    let refused = with_query_budget(Duration::ZERO, || wider().query_arc_outcome(&state));
    assert!(
        matches!(refused, Err(QueryLimit::QueryDeadline { millis: 0 })),
        "{refused:?}"
    );
    let answered = with_query_budget(Duration::from_secs(60), || {
        (
            rustel_core::installed_query_deadline_millis(),
            wide().query_arc_outcome(&state),
        )
    });
    assert!(
        matches!(answered, (Some(60_000), Ok(_))),
        "{:?}",
        answered.0
    );
    assert_eq!(rustel_core::installed_query_deadline_millis(), None);
}

#[test]
fn a_deadline_in_a_budget_never_reports_more_than_the_budget_allowed() {
    let inner = with_query_budget(Duration::from_millis(50), || {
        with_query_deadline(Instant::now() + Duration::from_secs(2), || {
            rustel_core::installed_query_deadline_millis()
        })
    });
    assert!(matches!(inner, Some(millis) if millis <= 50), "{inner:?}");
    // A budget in a deadline is a deadline: the tighter of the two.
    let inner = with_query_deadline(Instant::now() + Duration::from_millis(50), || {
        with_query_budget(Duration::from_secs(2), || {
            rustel_core::installed_query_deadline_millis()
        })
    });
    assert!(matches!(inner, Some(millis) if millis <= 50), "{inner:?}");
    // The budget around a deadline is in force again after it: a wide
    // query with no time is refused.
    let state = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
    let refused = with_query_budget(Duration::ZERO, || {
        with_query_deadline(Instant::now() + Duration::from_secs(2), || ());
        wider().query_arc_outcome(&state)
    });
    assert!(
        matches!(refused, Err(QueryLimit::QueryDeadline { .. })),
        "{refused:?}"
    );
}
