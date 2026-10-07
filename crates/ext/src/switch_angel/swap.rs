use rustel_core::Value;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::shared::value;
use super::{ORIGIN, strictly_equal};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "swap",
    "replace matching values",
    "Find-and-replace with strict equality: every receiver value equal to the first argument becomes the second, everything else passes through. Numbers and strings never compare equal.",
    params: [
        ReferenceParam {
            name: "find",
            r#type: "value",
            description: "the value to look for, compared strictly.",
        },
        ReferenceParam {
            name: "replacement",
            r#type: "value",
            description: "what matching values become; a missing argument replaces with undefined.",
        },
    ],
    examples: ["\"<0 1 2 1>\".swap(1, 5)"],
    "values"
);

fn apply<P: PatOps>(args: &[Value], pattern: P) -> P {
    let find = value(args, 0);
    let replacement = value(args, 1);
    pattern.fmap(move |held| {
        if strictly_equal(held, &find) {
            replacement.clone()
        } else {
            held.clone()
        }
    })
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["swap"],
        REFERENCE,
        3,
        false,
        rustel_core::native_combinator!(|args, pattern| apply(args, pattern)),
    );
}
