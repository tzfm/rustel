use rustel_core::Pattern;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

use super::ORIGIN;
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "p",
    "stack patterns",
    "Switch Angel's short global alias for stack: every pattern given plays at once.",
    params: [ReferenceParam {
        name: "patterns",
        r#type: "Pattern",
        description: "the patterns to play together; any number is accepted.",
    }],
    examples: ["p(note(\"c e g\").s(\"sawtooth\"), note(\"c2*2\").s(\"square\"))"],
    "composition"
);

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    rustel_core::stack(args.to_vec())
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["p"],
    arity: 0,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Stateless(apply),
};
