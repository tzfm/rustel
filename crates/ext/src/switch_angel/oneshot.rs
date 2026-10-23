use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::{number, pattern_argument};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "oneshot",
    "play once, on the next grid line",
    "Plays the pattern for one cycle, one time, on the next line of the grid. With a grid of 4, an update at cycle 37.3 plays cycle 40 and nothing after it.\n\nEach update reads the cycle again, so the pattern plays once more after every update that keeps the call. Delete the call when it has played. After Play, a restart or a rewind, the count starts at cycle 0, so the pattern plays in the first cycle.",
    params: [ReferenceParam {
        name: "grid",
        r#type: "number",
        description: "the size of the grid in cycles. With 4, the pattern plays on the next multiple of 4.",
    }],
    examples: ["note(\"c3 e3 g3 c4\").s(\"sawtooth\").oneshot(4)"],
    "time"
);

/// Keeps the events whose onset is in one cycle: the cycle that starts on
/// the first grid line at or after `cycle`.
fn once(receiver: &Pattern, cycle: f64, grid: &Value) -> Pattern {
    let grid = number(grid);
    let start = (cycle / grid).ceil() * grid;
    receiver.filter_haps(move |hap| {
        hap.whole.is_some_and(|whole| {
            let onset = whole.begin.to_f64();
            onset >= start && onset < start + 1.0
        })
    })
}

fn apply(cycle: f64, args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    let Some(receiver) = receiver.cloned() else {
        return rustel_core::silence();
    };
    let grid = pattern_argument(args, 0, Value::Undefined);
    match grid.as_pure() {
        Some(grid) => once(&receiver, cycle, &grid),
        None => grid.inner_bind(move |grid| once(&receiver, cycle, grid)),
    }
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["oneshot"],
    arity: 1,
    origin: ORIGIN,
    surface: CallableSurface::METHOD,
    behavior: PatternCallableBehavior::ReadCycle(apply),
};
