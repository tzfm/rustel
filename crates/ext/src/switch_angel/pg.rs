use rustel_core::Value;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{binary, scalar, source_control_pattern, value};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "pg",
    "multiply post-gain",
    "Multiplies the receiver's postgain control by the supplied value. A receiver with no postgain takes the value.",
    params: [ReferenceParam {
        name: "value",
        r#type: "number | Pattern",
        description: "the factor to multiply postgain by.",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").pg(0.8)"],
    "gain"
);

fn apply<P: PatOps>(args: &[Value], pattern: P) -> P {
    let postgain = source_control_pattern("postgain", scalar(value(args, 0)));
    binary(&pattern, &postgain, ComposeOp::Mul)
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["pg"],
        REFERENCE,
        2,
        false,
        rustel_core::native_combinator!(|args, pattern| apply(args, pattern)),
    );
}
