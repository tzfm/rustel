//! The kept answer tables of a process stay within their byte budget, and
//! a pattern past it answers as it does with room.

use rustel_core::{Pattern, Value, fastcat, pure, set_table_budget, table_bytes};
use rustel_fraction::Fraction;

fn sequence(offset: f64) -> Pattern {
    fastcat(
        (0..8)
            .map(|step| pure(Value::F64(offset + f64::from(step))))
            .collect(),
    )
}

fn answer(pattern: &Pattern) -> Vec<String> {
    pattern
        .query_arc(Fraction::ZERO, Fraction::new(4, 1))
        .iter()
        .map(|hap| format!("{:?} {}", hap.whole, hap.value.show()))
        .collect()
}

#[test]
fn kept_tables_stay_within_the_budget_and_answer_alike() {
    let roomy: Vec<Pattern> = (0..4).map(|n| sequence(f64::from(n))).collect();
    // A node builds its table the second time it is asked.
    let expected: Vec<Vec<String>> = roomy.iter().map(answer).collect();
    let again: Vec<Vec<String>> = roomy.iter().map(answer).collect();
    assert_eq!(again, expected);
    let needed = table_bytes();
    drop(roomy);
    assert_eq!(table_bytes(), 0, "dropped patterns give their tables back");

    let budget = needed / 2;
    assert!(budget > 0, "the patterns keep tables with room");
    set_table_budget(budget);
    let tight: Vec<Pattern> = (0..4).map(|n| sequence(f64::from(n))).collect();
    for _ in 0..3 {
        let answers: Vec<Vec<String>> = tight.iter().map(answer).collect();
        assert_eq!(answers, expected);
        assert!(table_bytes() <= budget, "{} bytes kept", table_bytes());
    }
    drop(tight);
    assert_eq!(table_bytes(), 0);
}
