use rustel_core::Value;
use rustel_core::compose::{Alignment, ComposeOp};
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{control, number, scalar};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "vstruct",
    "structure velocity from another pattern",
    "Uses one pattern for the event structure and writes its values as velocity: the receiver sounds where the ceiling of the structure value is non-zero, at exactly that value's velocity - so 0 drops the event and .5 plays it half as loud.",
    params: [ReferenceParam {
        name: "structure",
        r#type: "number | Pattern",
        description: "the gate-and-velocity pattern; values at or below 0 mute their span. Missing argument means silence.",
    }],
    examples: ["note(\"c e g a*4\").s(\"sawtooth\").vstruct(\"<1 .5 0 .8>\")"],
    "rhythm"
);

fn apply<P: PatOps>(structure: &P, pattern: &P) -> P {
    let pattern = pattern.clone();
    structure.outer_bind(move |velocity| {
        let velocity = number(velocity);
        let mask = scalar(Value::F64(velocity.ceil()));
        let kept =
            rustel_core::compose::compose(&pattern, &mask, ComposeOp::KeepIf, Alignment::Out);
        control(&kept, "velocity", &scalar(Value::F64(velocity)))
    })
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["vstruct"],
        REFERENCE,
        2,
        false,
        rustel_core::native_patterned_combinator!(|args, pattern| {
            let Some(structure) = args.first() else {
                return PatOps::pat_silence();
            };
            apply(structure, &pattern)
        }),
    );
}
