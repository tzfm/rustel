use rustel_core::Pattern;
use rustel_core::compose::{Alignment, ComposeOp};
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

use super::ORIGIN;
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "overin",
    "set values with inner alignment",
    "Sets the receiver's values from a stack of its arguments using inner alignment, so the result keeps the shorter of the two spans.",
    params: [ReferenceParam {
        name: "values",
        r#type: "Pattern",
        description: "one or more patterns whose stack overwrites the receiver's values; aligned to the inner span.",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").overin(n(\"0 3 7\"))"],
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
        Alignment::In,
    )
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["overin"],
    arity: 0,
    origin: ORIGIN,
    surface: CallableSurface::METHOD,
    behavior: PatternCallableBehavior::Stateless(apply),
};
