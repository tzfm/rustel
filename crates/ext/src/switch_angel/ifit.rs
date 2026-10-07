use rustel_core::Value;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{clip, fraction, value};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "ifit",
    "fit a pattern inside a cycle count",
    "Slows the receiver to the given number of cycles, fits it, and speeds it back, then clips every event to at most one cycle - a whole pattern squeezed into a window without spilling over.",
    params: [ReferenceParam {
        name: "cycles",
        r#type: "number | Pattern",
        description: "how many cycles the fitted pattern spans; a missing or non-numeric argument acts as zero and squeezes to silence.",
    }],
    examples: ["note(\"c e g a*4\").s(\"sawtooth\").ifit(2)"],
    "time"
);

fn apply<P: PatOps>(args: &[Value], pattern: P) -> P {
    let cycles = fraction(&value(args, 0));
    let fitted = pattern.slow(cycles).fit().fast(cycles);
    clip(&fitted.slow(cycles), 1.0)
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["ifit"],
        REFERENCE,
        2,
        false,
        rustel_core::native_combinator!(|args, pattern| apply(args, pattern)),
    );
}
