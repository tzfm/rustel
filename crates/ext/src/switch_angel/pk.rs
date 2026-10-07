use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "pk",
    "pick from patterns",
    "Selects among its pattern arguments. With more than two arguments the final one is the selector; with one or two arguments everything is a choice and the selector defaults to 0, so the first choice plays. Out-of-range selectors clamp to the ends.",
    params: [
        ReferenceParam {
            name: "choices",
            r#type: "Pattern",
            description: "the patterns to pick between.",
        },
        ReferenceParam {
            name: "selector",
            r#type: "number | Pattern",
            description: "index of the choice; only read when more than two arguments are given, defaults to 0, and clamps to the available choices.",
        },
    ],
    examples: ["pk(note(\"c e g\").s(\"sawtooth\"), note(\"d f a\").s(\"square\"), \"<0 1>\")"],
    "composition"
);

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    let (choices, selector) = if args.len() > 2 {
        (&args[..args.len() - 1], args.last().cloned().unwrap())
    } else {
        (args, rustel_core::pure(Value::F64(0.0)))
    };
    let lookup = rustel_core::PickLookup::Array {
        enumerable_len: choices.len(),
        length: choices.len(),
        entries: choices.iter().cloned().enumerate().collect(),
    };
    rustel_core::pick(
        selector,
        lookup,
        rustel_core::PickIndexMode::Clamp,
        rustel_core::JoinMode::Inner,
    )
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["pk"],
    arity: 0,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Stateless(apply),
};
