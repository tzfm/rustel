use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::pattern_argument;
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "irando",
    "integer random pattern with outer join",
    "Patterned random integers flattened with the outer timing, so each bound keeps its own event span.",
    params: [ReferenceParam {
        name: "n",
        r#type: "number | Pattern",
        description: "the number of possible values; integers run from 0 up to n. Missing argument yields nothing.",
    }],
    examples: ["irando(8)"],
    "random"
);

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    pattern_argument(args, 0, Value::Undefined)
        .fmap_to_pattern(|bound| rustel_core::signal::irand_value(bound.clone()))
        .outer_join()
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["irando"],
    arity: 1,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Stateless(apply),
};
