use rustel_core::compose::ComposeOp;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::{pattern_argument, pattern_binary};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "randm",
    "quantized random fractions",
    "Random integers from 0 up to the division, divided back by it - quantized random fractions between 0 and 1 that step on the chosen grid.",
    params: [ReferenceParam {
        name: "division",
        r#type: "number | Pattern",
        description: "the grid size; also the exclusive upper bound of the random integers. A missing argument yields nothing.",
    }],
    examples: ["randm(8)"],
    "random"
);

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    let division = pattern_argument(args, 0, Value::Undefined);
    pattern_binary(
        &rustel_core::signal::irand(&division),
        &division,
        ComposeOp::Div,
    )
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["randm"],
    arity: 1,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Stateless(apply),
};
