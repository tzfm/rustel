use rustel_core::Pattern;
use rustel_core::compose::ComposeOp;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

use super::ORIGIN;
use super::shared::keep_or_bound;
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "min",
    "clamp values to a minimum",
    "Floors the receiver from below: a value greater than the bound passes through and any other value becomes the bound. A value and bound that do not compare, such as a word and a number, give the bound.",
    params: [ReferenceParam {
        name: "bound",
        r#type: "number | Pattern",
        description: "the lower bound; values below it are lifted up to it.",
    }],
    examples: ["\"<0 4 8 12>\".min(4)"],
    "values"
);

fn apply(args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    keep_or_bound(args, receiver, ComposeOp::Gt)
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["min"],
    arity: 1,
    origin: ORIGIN,
    surface: CallableSurface::METHOD,
    behavior: PatternCallableBehavior::Stateless(apply),
};
