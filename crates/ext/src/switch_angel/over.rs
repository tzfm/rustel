use rustel_core::Pattern;
use rustel_core::compose::{Alignment, ComposeOp};
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

use super::ORIGIN;
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "over",
    "set values with outer alignment",
    "Sets the receiver's values from a stack of its arguments using outer alignment, so the result spans the longer of the two.",
    params: [ReferenceParam {
        name: "values",
        r#type: "Pattern",
        description: "one or more patterns whose stack overwrites the receiver's values; aligned to the outer span.",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").over(n(\"0 3 7\"))"],
    "composition"
);

fn apply(args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    let Some(receiver) = receiver else {
        return rustel_core::silence();
    };
    rustel_core::compose::compose(
        receiver,
        &rustel_core::stack(args.to_vec()),
        ComposeOp::Set,
        Alignment::Out,
    )
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["over"],
    arity: 0,
    origin: ORIGIN,
    surface: CallableSurface::METHOD,
    behavior: PatternCallableBehavior::Stateless(apply),
};
