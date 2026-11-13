//! The seams of one score's life in the engine, each measured on its own.

use crate::measure::{Cost, measure};
use rustel_fraction::Fraction;
use rustel_runtime::{Session, SessionConfig};

/// How hard a run measures each score.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    /// Fresh sessions for the seams of a cold start, taken as a median.
    rounds: usize,
    /// Queries of the same span in a row.
    steady_queries: usize,
    /// One-eighth-cycle queries in a row, as playback asks.
    windows: usize,
}

impl Plan {
    pub const QUICK: Self = Self {
        rounds: 1,
        steady_queries: 20,
        windows: 64,
    };
    pub const FULL: Self = Self {
        rounds: 5,
        steady_queries: 300,
        windows: 512,
    };
}

/// One measured seam: its name and its cost.
pub type Seam = (&'static str, Cost);

/// The span a whole-score query asks: sixteen cycles from zero.
fn whole() -> (Fraction, Fraction) {
    (Fraction::ZERO, Fraction::new(16, 1))
}

/// Measure every seam of `source`. `Err` when the score does not evaluate.
pub fn measure_score(source: &str, plan: Plan) -> Result<Vec<Seam>, String> {
    let mut cold: Vec<[Cost; 3]> = Vec::with_capacity(plan.rounds);
    let mut kept = None;
    for _ in 0..plan.rounds.max(1) {
        let (session, boot) = measure(|| Session::with_config(SessionConfig::default()));
        let mut session = session.map_err(|error| error.to_string())?;
        let (evaluated, eval) = measure(|| session.evaluate(source));
        evaluated.map_err(|error| error.to_string())?;
        let (_, first) = measure(|| query(&session, whole()));
        cold.push([boot, eval, first]);
        kept = Some(session);
    }
    let mut session = kept.expect("at least one round");
    let column =
        |index: usize| Cost::median(&cold.iter().map(|costs| costs[index]).collect::<Vec<_>>());
    let mut seams = vec![
        ("boot", column(0)),
        ("cold eval", column(1)),
        ("first query", column(2)),
    ];

    let (_, steady) = measure(|| {
        for _ in 0..plan.steady_queries {
            query(&session, whole());
        }
    });
    seams.push(("steady query", steady.per(plan.steady_queries)));

    let eighth = Fraction::new(1, 8);
    let mut worst = Cost::default();
    let (_, windows) = measure(|| {
        let mut begin = Fraction::ZERO;
        for _ in 0..plan.windows {
            let end = begin
                .checked_add(eighth)
                .expect("a window inside the native range");
            let (_, window) = measure(|| query(&session, (begin, end)));
            if window.wall_ns > worst.wall_ns {
                worst = window;
            }
            begin = end;
        }
    });
    seams.push(("window query", windows.per(plan.windows)));
    seams.push(("worst window", worst));

    let (evaluated, again) = measure(|| session.evaluate(source));
    evaluated.map_err(|error| error.to_string())?;
    seams.push(("re-eval", again));
    seams.push((
        "query after re-eval",
        measure(|| query(&session, whole())).1,
    ));

    let edited = edited(source);
    let (evaluated, edit) = measure(|| session.evaluate(&edited));
    if evaluated.is_ok() {
        seams.push(("edit", edit));
        seams.push(("query after edit", measure(|| query(&session, whole())).1));
    }

    // What the live session keeps: its tables, then everything it frees
    // when it drops.
    seams.push((
        "tables held",
        Cost {
            held: i64::try_from(rustel_core::table_bytes()).unwrap_or(i64::MAX),
            ..Cost::default()
        },
    ));
    let (_, dropped) = measure(|| drop(session));
    seams.push((
        "session held",
        Cost {
            held: -dropped.held,
            ..Cost::default()
        },
    ));
    Ok(seams)
}

/// Query `span`, keeping only the count so the haps drop inside the seam.
fn query(session: &Session, (begin, end): (Fraction, Fraction)) -> usize {
    session.query(begin, end).map_or(0, |haps| haps.len())
}

/// `source` with one edit a live coder makes: its last digit changed.
/// A score with no digit gets a line comment instead.
fn edited(source: &str) -> String {
    let mut text = source.to_owned();
    match text.rfind(|c: char| c.is_ascii_digit()) {
        Some(at) => {
            let digit = text.as_bytes()[at] - b'0';
            let next = char::from(b'0' + (digit + 1) % 10);
            text.replace_range(at..=at, next.encode_utf8(&mut [0; 4]));
        }
        None => text.push_str("\n// edit\n"),
    }
    text
}

#[cfg(test)]
mod tests {
    use super::edited;

    #[test]
    fn an_edit_changes_the_last_digit_or_adds_a_comment() {
        assert_eq!(edited("note(\"0 2 4\")"), "note(\"0 2 5\")");
        assert_eq!(edited("s(\"bd\").gain(0.9)"), "s(\"bd\").gain(0.0)");
        assert_eq!(edited("s(\"bd\")"), "s(\"bd\")\n// edit\n");
    }
}
